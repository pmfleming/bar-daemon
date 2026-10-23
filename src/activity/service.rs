use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{Local, TimeZone};
use tokio::sync::{Mutex, RwLock};

use crate::{state::StateStore, time::unix_ms_i64 as unix_ms};

use super::{
    config::{self, ActivityConfig},
    model::{
        ActivityEvent, ActivityRange, ActivitySourceState, ActivityState, TodoItem, WeatherState,
        WorldClockState,
    },
    provider::ProviderRegistry,
    weather,
};

#[derive(Default)]
struct ActivityData {
    config: ActivityConfig,
    events_by_source: HashMap<String, Vec<ActivityEvent>>,
    source_states: HashMap<String, ActivitySourceState>,
    todos: Vec<TodoItem>,
    // A failed load must never turn into an empty, writable store.
    todo_error: Option<String>,
    weather: WeatherState,
    weather_locations: Vec<WeatherState>,
    catalog: Option<super::catalog::Catalog>,
    catalog_error: Option<String>,
}

impl ActivityData {
    fn writable_catalog(&self) -> Result<&super::catalog::Catalog> {
        anyhow::ensure!(
            self.catalog_error.is_none(),
            "City store unavailable; restart after resolving storage failure"
        );
        self.catalog.as_ref().context("City store unavailable")
    }
}

pub(crate) struct ActivityService {
    data: RwLock<ActivityData>,
    state: StateStore,
    config_path: PathBuf,
    todo_path: PathBuf,
    catalog_path: PathBuf,
    providers: ProviderRegistry,
    refresh_guard: Mutex<()>,
    publication_guard: Mutex<()>,
    todo_guard: Mutex<()>,
    todo_sequence: AtomicU64,
}

impl ActivityService {
    pub(crate) async fn new(state: StateStore) -> Arc<Self> {
        Self::with_paths(state, config::config_path(), config::todo_path()).await
    }

    pub(crate) async fn with_paths(
        state: StateStore,
        config_path: PathBuf,
        todo_path: PathBuf,
    ) -> Arc<Self> {
        let (todos, todo_error) = match config::load_todos(&todo_path).await {
            Ok(todos) => (todos, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Local todo store is unavailable: {error:#}")),
            ),
        };
        let catalog_path = todo_path.with_file_name("locations.json");
        let catalog_result = async {
            // Once imported, a malformed calendar config cannot replace saved cities.
            let config = if tokio::fs::try_exists(&catalog_path).await? {
                ActivityConfig::default()
            } else {
                config::load(&config_path).await?
            };
            super::catalog::Catalog::load_or_import(&catalog_path, &config).await
        }
        .await;
        let (catalog, catalog_error) = match catalog_result {
            Ok(catalog) => (Some(catalog), None),
            Err(error) => (
                Some(super::catalog::Catalog::protected_home()),
                Some(format!("City store unavailable: {error:#}")),
            ),
        };
        let service = Arc::new(Self {
            data: RwLock::new(ActivityData {
                todos,
                todo_error,
                catalog,
                catalog_error,
                ..ActivityData::default()
            }),
            state,
            config_path,
            todo_path,
            catalog_path,
            providers: ProviderRegistry::builtins(),
            refresh_guard: Mutex::new(()),
            publication_guard: Mutex::new(()),
            todo_guard: Mutex::new(()),
            todo_sequence: AtomicU64::new(1),
        });
        service.publish_state(None).await;
        service
    }

    // Caller holds todo_guard so recovery cannot replace an in-flight mutation.
    // Retry only failed loads; successful in-memory state remains authoritative.
    async fn writable_todos(&self) -> Result<Vec<TodoItem>> {
        let data = self.data.read().await;
        if data.todo_error.is_none() {
            return Ok(data.todos.clone());
        }
        drop(data);
        match config::load_todos(&self.todo_path).await {
            Ok(todos) => {
                let mut data = self.data.write().await;
                data.todos = todos.clone();
                data.todo_error = None;
                Ok(todos)
            }
            Err(error) => {
                let message = format!("Local todo store is unavailable: {error:#}");
                self.data.write().await.todo_error = Some(message.clone());
                self.publish_state(None).await;
                bail!(message)
            }
        }
    }

    pub(crate) async fn monitor(self: Arc<Self>) {
        tokio::select! {
            _ = Arc::clone(&self).monitor_sources() => {},
            _ = self.monitor_clock() => {},
        }
    }

    async fn monitor_clock(&self) {
        loop {
            tokio::time::sleep(super::presentation::next_minute(chrono::Utc::now())).await;
            self.publish_clock(chrono::Utc::now()).await;
        }
    }

    async fn publish_clock(&self, now: chrono::DateTime<chrono::Utc>) {
        // Independent of provider I/O; publication shares the existing guard.
        let _publication = self.publication_guard.lock().await;
        let mut state = self.state.read(|s| s.activity.clone()).await;
        super::presentation::update(&mut state, now);
        self.state.update_activity(state).await;
    }

    async fn monitor_sources(self: Arc<Self>) {
        let mut events = self
            .state
            .subscribe_domains(&[crate::protocol::stream::TIMEZONE]);
        self.refresh().await;
        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await;
        loop {
            tokio::select! {
                _ = refresh.tick() => self.refresh().await,
                event = events.recv() => match event {
                    Ok(event) if event.stream == crate::protocol::stream::TIMEZONE => self.publish_state(None).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => self.publish_state(None).await,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    _ => {},
                }
            }
        }
    }

    async fn publish_syncing(&self) {
        let _publication = self.publication_guard.lock().await;
        let mut current = self.state.read(|s| s.activity.clone()).await;
        current.available = true;
        current.syncing = true;
        self.state.update_activity(current).await;
    }

    pub(crate) async fn request_refresh(self: &Arc<Self>) {
        self.publish_syncing().await;
        let service = Arc::clone(self);
        tokio::spawn(async move { service.refresh().await });
    }

    pub(crate) async fn refresh(&self) {
        let Ok(_guard) = self.refresh_guard.try_lock() else {
            return;
        };
        self.publish_syncing().await;

        {
            let _guard = self.todo_guard.lock().await;
            let needs_recovery = self.data.read().await.todo_error.is_some();
            if needs_recovery && let Err(error) = self.writable_todos().await {
                tracing::warn!(%error, "local todo store recovery failed");
            }
        }

        let config = match config::load(&self.config_path).await {
            Ok(config) => config,
            Err(error) => {
                self.publish_state(Some(error.to_string())).await;
                return;
            }
        };

        let configured_ids = config
            .calendar_sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<BTreeSet<_>>();
        {
            let mut data = self.data.write().await;
            // Saved cities are imported once. Config refresh must not resurrect removals.
            data.config = config.clone();
            data.events_by_source
                .retain(|source_id, _| configured_ids.contains(source_id.as_str()));
            data.source_states
                .retain(|source_id, _| configured_ids.contains(source_id.as_str()));
        }

        let mut first_error = None;
        let calendar_results = futures::future::join_all(
            config
                .calendar_sources
                .iter()
                .map(|source| async move { (source, self.providers.load(source).await) }),
        )
        .await;
        for (source, result) in calendar_results {
            let mut data = self.data.write().await;
            let error = match result {
                Ok(events) => {
                    data.events_by_source.insert(source.id.clone(), events);
                    None
                }
                Err(error) => Some(error.to_string()),
            };
            first_error = first_error.or_else(|| error.clone());
            let item_count = data
                .events_by_source
                .get(&source.id)
                .map_or(0, Vec::len)
                .try_into()
                .unwrap_or(u32::MAX);
            data.source_states.insert(
                source.id.clone(),
                ActivitySourceState {
                    id: source.id.clone(),
                    name: source.display_name().into(),
                    kind: source.kind.clone(),
                    available: error.is_none(),
                    item_count,
                    error,
                },
            );
        }
        self.refresh_weather(&config).await;
        self.publish_state(first_error).await;
    }

    async fn refresh_weather(&self, config: &ActivityConfig) {
        let owned = {
            let data = self.data.read().await;
            data.catalog.as_ref().map(|c| c.weather_configs())
        };
        let locations = owned
            .as_deref()
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| config.configured_weather_locations());
        if locations.is_empty() {
            let mut data = self.data.write().await;
            data.weather = WeatherState {
                location: "Local".into(),
                error: Some("Weather is not configured".into()),
                ..WeatherState::default()
            };
            data.weather_locations.clear();
            return;
        }
        let mut cached = self.data.read().await.weather_locations.clone();
        let now = unix_ms();
        let forecasts = futures::future::join_all(locations.iter().map(|location| {
            // Config validation guarantees unique IDs. Move each cached forecast
            // into its refresh, while readers retain the last published snapshot.
            let previous = cached
                .iter()
                .position(|weather| weather.id == location.id)
                .map(|index| cached.swap_remove(index));
            weather::cached_or_fetch(location, previous, now, async {
                // Service unit fixtures never contact live forecast providers.
                if cfg!(test) {
                    anyhow::bail!("Forecast provider disabled in unit fixture")
                } else {
                    tokio::time::timeout(Duration::from_secs(15), weather::fetch(location))
                        .await
                        .context("Forecast request timed out")?
                }
            })
        }))
        .await;
        let primary = forecasts
            .iter()
            .find(|weather| weather.home)
            .or_else(|| forecasts.first())
            .cloned()
            .unwrap_or_default();
        let mut data = self.data.write().await;
        data.weather = primary;
        data.weather_locations = forecasts;
    }

    pub(crate) async fn add_city(
        self: &Arc<Self>,
        place: crate::location_place::Place,
        revision: u64,
    ) -> Result<String> {
        let guard = self.refresh_guard.lock().await;
        let data = self.data.read().await;
        let catalog = data.writable_catalog()?;
        let (next, id) = catalog.add(place, revision)?;
        let changed = next.revision != catalog.revision;
        drop(data);
        if changed {
            self.save_catalog(next).await?;
        }
        drop(guard);
        self.publish_state(None).await;
        self.request_refresh().await;
        Ok(id)
    }
    pub(crate) async fn remove_city(&self, id: &str, revision: u64) -> Result<()> {
        let _guard = self.refresh_guard.lock().await;
        let next = {
            let data = self.data.read().await;
            data.writable_catalog()?.remove(id, revision)?
        };
        self.save_catalog(next).await?;
        self.data
            .write()
            .await
            .weather_locations
            .retain(|w| w.id != id);
        self.publish_state(None).await;
        Ok(())
    }
    async fn save_catalog(&self, catalog: super::catalog::Catalog) -> Result<()> {
        // A failed durability barrier may have replaced the file. Fence writes
        // until restart/reload rather than publishing an unconfirmed revision.
        if let Err(error) = crate::paths::save_json_atomic(&self.catalog_path, &catalog).await {
            let mut data = self.data.write().await;
            data.catalog_error = Some(format!("City save unconfirmed: {error:#}"));
            return Err(error);
        }
        self.data.write().await.catalog = Some(catalog);
        Ok(())
    }

    pub(crate) async fn query_range(
        &self,
        from_unix_ms: i64,
        to_unix_ms: i64,
    ) -> Result<ActivityRange> {
        self.query_range_in_timezone(from_unix_ms, to_unix_ms, Local)
            .await
    }

    async fn query_range_in_timezone<T: TimeZone>(
        &self,
        from_unix_ms: i64,
        to_unix_ms: i64,
        timezone: T,
    ) -> Result<ActivityRange> {
        let range = super::day::QueryRange::new(from_unix_ms, to_unix_ms, &timezone)?;
        let data = self.data.read().await;
        let mut events = data
            .events_by_source
            .values()
            .flatten()
            .filter(|event| range.includes_event(event))
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(ActivityEvent::chronological_cmp);
        let mut todos = data
            .todos
            .iter()
            .filter(|todo| range.includes_todo(todo))
            .cloned()
            .collect::<Vec<_>>();
        todos.sort_by_key(|todo| {
            (
                todo.completed,
                todo.due_unix_ms.unwrap_or(i64::MAX),
                std::cmp::Reverse(todo.priority),
                todo.created_unix_ms,
            )
        });
        let today = chrono::Utc::now().with_timezone(&timezone).date_naive();
        let days = super::day::project(&events, &todos, range.first, range.last, today, &timezone);
        let busy_dates = days
            .iter()
            .filter(|(_, day)| !day.event_ids.is_empty() || !day.todo_ids.is_empty())
            .map(|(date, _)| date.clone())
            .collect();
        Ok(ActivityRange {
            from_unix_ms,
            to_unix_ms,
            events,
            todos,
            busy_dates,
            days,
            local_date: today.to_string(),
        })
    }

    pub(crate) async fn create_todo(
        &self,
        title: String,
        due_unix_ms: Option<i64>,
        due_date: Option<String>,
        priority: u8,
    ) -> Result<TodoItem> {
        let title = title.trim();
        if title.is_empty() {
            bail!("todo title cannot be empty");
        }
        if priority > 9 {
            bail!("todo priority must be between 0 and 9");
        }
        if let Some(date) = due_date.as_deref() {
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .with_context(|| format!("parse todo due date {date}"))?;
        }
        let now = unix_ms();
        self.mutate_todos(|todos| {
            let todo = TodoItem {
                id: format!(
                    "local-{now}-{}",
                    self.todo_sequence.fetch_add(1, Ordering::Relaxed)
                ),
                source_id: "local".into(),
                title: title.into(),
                completed: false,
                priority,
                due_unix_ms,
                due_date,
                created_unix_ms: now,
                completed_unix_ms: None,
            };
            todos.push(todo.clone());
            Ok(todo)
        })
        .await
    }

    pub(crate) async fn complete_todo(&self, id: &str, completed: bool) -> Result<TodoItem> {
        self.mutate_todos(|todos| {
            let todo = todos
                .iter_mut()
                .find(|todo| todo.id == id)
                .with_context(|| format!("todo {id} was not found"))?;
            todo.completed = completed;
            todo.completed_unix_ms = completed.then(unix_ms);
            Ok(todo.clone())
        })
        .await
    }

    pub(crate) async fn delete_todo(&self, id: &str) -> Result<()> {
        self.mutate_todos(|todos| {
            let previous = todos.len();
            todos.retain(|todo| todo.id != id);
            anyhow::ensure!(todos.len() != previous, "todo {id} was not found");
            Ok(())
        })
        .await
    }

    // Keep the published snapshot intact until the candidate is durably saved.
    async fn mutate_todos<T>(
        &self,
        mutate: impl FnOnce(&mut Vec<TodoItem>) -> Result<T>,
    ) -> Result<T> {
        let _guard = self.todo_guard.lock().await;
        let mut todos = self.writable_todos().await?;
        let result = mutate(&mut todos)?;
        config::save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None).await;
        Ok(result)
    }

    async fn publish_state(&self, error: Option<String>) {
        let _publication = self.publication_guard.lock().await;
        let data = self.data.read().await;
        let now = unix_ms();
        let events = data.events_by_source.values().flatten();
        let next_event = events
            .clone()
            .filter(|event| event.end_unix_ms >= now)
            .min_by_key(|event| event.start_unix_ms)
            .cloned();
        let sources: Vec<ActivitySourceState> = data
            .config
            .calendar_sources
            .iter()
            .map(|source| {
                data.source_states
                    .get(&source.id)
                    .cloned()
                    .unwrap_or_else(|| ActivitySourceState {
                        id: source.id.clone(),
                        name: source.display_name().into(),
                        kind: source.kind.clone(),
                        ..ActivitySourceState::default()
                    })
            })
            .collect();
        let world_clocks = data
            .config
            .world_clocks
            .iter()
            .filter_map(|clock| WorldClockState::new(&clock.timezone, &clock.label).ok())
            .collect();
        let catalog = data.catalog.clone();
        let error = data
            .catalog_error
            .clone()
            .or_else(|| data.todo_error.clone())
            .or(error)
            .or_else(|| {
                sources
                    .iter()
                    .find_map(|source: &ActivitySourceState| source.error.clone())
            });
        let mut state = ActivityState {
            available: true,
            syncing: false,
            event_count: events.count().try_into().unwrap_or(u32::MAX),
            incomplete_todo_count: data
                .todos
                .iter()
                .filter(|todo| !todo.completed)
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
            next_event,
            sources,
            world_clocks,
            locations: Vec::new(),
            location_catalog: catalog.as_ref().map(|c| super::catalog::CatalogInfo {
                revision: c.revision,
                writable: data.catalog_error.is_none(),
            }),
            lunar: super::astronomy::lunar(now),
            weather: super::astronomy::weather(data.weather.clone(), now),
            weather_locations: data
                .weather_locations
                .iter()
                .cloned()
                .map(|weather| super::astronomy::weather(weather, now))
                .collect(),
            error,
        };
        drop(data);
        let local = self.state.read(|s| s.timezone.clone()).await;
        if let Some(catalog) = catalog {
            state.locations = catalog.project(&state.weather_locations, chrono::Utc::now());
            for location in &mut state.locations {
                location.lunar = state.lunar.clone();
            }
            super::locations::append_current(&mut state.locations, &local, chrono::Utc::now());
        } else {
            state.locations = super::locations::project(&state, &local);
        }
        super::presentation::update(&mut state, chrono::Utc::now());
        self.state.update_activity(state).await;
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::state::StateStore;

    use super::ActivityService;

    #[tokio::test]
    async fn clock_publication_preserves_provider_outcomes_and_catalog_identity() {
        use chrono::TimeZone;
        let directory = tempdir().unwrap();
        let service = service_in(&directory).await;
        let mut initial = service.state.read(|s| s.activity.clone()).await;
        initial.error = Some("Provider unavailable".into());
        initial.syncing = true;
        let ids: Vec<_> = initial.locations.iter().map(|l| l.id.clone()).collect();
        let catalog = initial.location_catalog.clone();
        service.state.update_activity(initial).await;
        service
            .publish_clock(chrono::Utc.with_ymd_and_hms(2026, 3, 29, 1, 0, 0).unwrap())
            .await;
        let published = service.state.read(|s| s.activity.clone()).await;
        assert_eq!(published.error.as_deref(), Some("Provider unavailable"));
        assert!(published.syncing);
        assert_eq!(published.location_catalog, catalog);
        assert_eq!(
            published
                .locations
                .iter()
                .map(|l| l.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            published
                .locations
                .iter()
                .find(|l| l.home)
                .unwrap()
                .clock
                .clock_display["time"],
            "03:00"
        );
    }
    #[tokio::test]
    async fn city_catalog_survives_bad_config_and_corruption_never_moves_home() {
        let directory = tempdir().unwrap();
        let service = service_in(&directory).await;
        let mut place = crate::activity::locations::home_place();
        place.name = "Haarlem".into();
        place.latitude = 52.38;
        place.longitude = 4.64;
        let id = service.add_city(place, 1).await.unwrap();
        {
            let _guard = service.refresh_guard.lock().await;
        }
        std::fs::write(directory.path().join("activity.json"), b"bad-calendar-json").unwrap();
        let reopened = service_in(&directory).await;
        assert!(
            reopened
                .state
                .read(|s| s.activity.locations.iter().any(|c| c.id == id))
                .await
        );
        reopened.remove_city(&id, 2).await.unwrap();
        std::fs::write(directory.path().join("locations.json"), b"broken").unwrap();
        let broken = service_in(&directory).await;
        let snapshot = broken.state.read(|s| s.activity.clone()).await;
        let home = snapshot.locations.iter().find(|c| c.home).unwrap();
        assert_eq!(home.clock.label, "Amsterdam");
        assert_eq!(home.clock.timezone, "Europe/Amsterdam");
        assert!(!snapshot.location_catalog.unwrap().writable);
        assert!(
            broken
                .add_city(crate::activity::locations::home_place(), 1)
                .await
                .is_err()
        );
    }

    async fn service_in(directory: &tempfile::TempDir) -> std::sync::Arc<ActivityService> {
        ActivityService::with_paths(
            StateStore::default(),
            directory.path().join("activity.json"),
            directory.path().join("todos.json"),
        )
        .await
    }

    #[tokio::test]
    async fn failed_todo_storage_blocks_mutations_and_recovers_without_data_loss() {
        for unreadable in [false, true] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("todos.json");
            let original = b"[{\"id\":\"recoverable-task\",\"title\":\"Important\"}";
            if unreadable {
                // A directory gives a deterministic read error even as root.
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::write(&path, original).unwrap();
            }
            let service = service_in(&directory).await;
            let state = &service.state;
            assert!(state.snapshot().await.activity.error.is_some());
            assert!(
                service
                    .create_todo("New".into(), None, None, 0)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .complete_todo("recoverable-task", true)
                    .await
                    .is_err()
            );
            assert!(service.delete_todo("recoverable-task").await.is_err());
            service.refresh().await;
            assert!(state.snapshot().await.activity.error.is_some());
            if unreadable {
                assert!(path.is_dir());
                std::fs::remove_dir(&path).unwrap();
            } else {
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }

            let recovered = serde_json::json!([{
                "id": "recoverable-task", "source_id": "local", "title": "Important",
                "completed": false, "priority": 0, "due_unix_ms": null, "due_date": null,
                "created_unix_ms": 1, "completed_unix_ms": null
            }]);
            std::fs::write(&path, recovered.to_string()).unwrap();
            if unreadable {
                service.refresh().await; // Automatic recovery, without restart.
                assert!(state.snapshot().await.activity.error.is_none());
            }
            // Mutation also retries a failed load and preserves recovered data.
            service
                .create_todo("New".into(), None, None, 0)
                .await
                .unwrap();
            let stored = super::config::load_todos(&path).await.unwrap();
            assert_eq!(stored.len(), 2);
            assert_eq!(stored[0].id, "recoverable-task");
            assert!(state.snapshot().await.activity.error.is_none());

            // A recovered load must not allow a later failed save to publish.
            let before = state.snapshot().await.activity;
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert!(
                service
                    .create_todo("Lost".into(), None, None, 0)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .complete_todo("recoverable-task", true)
                    .await
                    .is_err()
            );
            assert!(service.delete_todo("recoverable-task").await.is_err());
            assert_eq!(service.data.read().await.todos, stored);
            assert_eq!(state.snapshot().await.activity, before);
            std::fs::remove_dir(&path).unwrap();
            service
                .complete_todo("recoverable-task", true)
                .await
                .unwrap();
            assert!(super::config::load_todos(&path).await.unwrap()[0].completed);
        }
    }

    #[tokio::test]
    async fn date_only_queries_use_local_half_open_days_including_dst_and_partial_days() {
        use chrono::{DateTime, TimeZone};
        let directory = tempdir().unwrap();
        let config = directory.path().join("activity.json");
        let todos = directory.path().join("new/todos.json");
        let service =
            ActivityService::with_paths(StateStore::default(), config.clone(), todos.clone()).await;
        let todo = service
            .create_todo("Keep me".into(), None, None, 3)
            .await
            .unwrap();
        let service = ActivityService::with_paths(StateStore::default(), config, todos).await;
        assert_eq!(
            service.query_range(0, 1).await.unwrap().todos,
            vec![todo.clone()]
        );
        assert!(
            service
                .complete_todo(&todo.id, true)
                .await
                .unwrap()
                .completed
        );
        service.delete_todo(&todo.id).await.unwrap();
        assert!(service.query_range(0, 1).await.unwrap().todos.is_empty());
        const MAX_RANGE_MS: i64 = 370 * 24 * 60 * 60 * 1_000;
        for (from, to) in [
            (10, 10),
            (i64::MIN, i64::MAX),
            (i64::MIN, 0),
            (0, MAX_RANGE_MS + 1),
        ] {
            assert!(service.query_range(from, to).await.is_err());
        }
        assert!(service.query_range(0, MAX_RANGE_MS).await.is_ok());
        for (zone, from, to, date) in [
            (
                chrono_tz::Africa::Johannesburg,
                "2026-01-20T00:00:00+02:00",
                "2026-01-21T00:00:00+02:00",
                "2026-01-20",
            ),
            (
                chrono_tz::America::New_York,
                "2026-01-20T00:00:00-05:00",
                "2026-01-21T00:00:00-05:00",
                "2026-01-20",
            ),
            (
                chrono_tz::UTC,
                "2026-01-20T09:00:00Z",
                "2026-01-20T10:00:00Z",
                "2026-01-20",
            ),
            (
                chrono_tz::Europe::Berlin,
                "2026-03-29T00:00:00+01:00",
                "2026-03-30T00:00:00+02:00",
                "2026-03-29",
            ),
            (
                chrono_tz::Europe::Berlin,
                "2026-10-25T00:00:00+02:00",
                "2026-10-26T00:00:00+01:00",
                "2026-10-25",
            ),
        ] {
            let from = DateTime::parse_from_rfc3339(from)
                .unwrap()
                .timestamp_millis();
            let to = DateTime::parse_from_rfc3339(to).unwrap().timestamp_millis();
            let first = zone.timestamp_millis_opt(from).unwrap().date_naive();
            let last = zone.timestamp_millis_opt(to - 1).unwrap().date_naive();
            let mut ids = Vec::new();
            for day in [
                first.pred_opt().unwrap().to_string(),
                date.into(),
                last.succ_opt().unwrap().to_string(),
            ] {
                ids.push(
                    service
                        .create_todo(day.clone(), None, Some(day), 0)
                        .await
                        .unwrap()
                        .id,
                );
            }
            let range = service
                .query_range_in_timezone(from, to, zone)
                .await
                .unwrap();
            assert_eq!(range.todos.len(), 1, "{zone}: {date}");
            assert_eq!(range.todos[0].id, ids[1]);
            for id in ids {
                service.delete_todo(&id).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn refreshes_multiple_local_calendar_sources() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.ics");
        let second = directory.path().join("second.ics");
        let now = chrono::Utc::now();
        let today = now.date_naive();
        let first_date = today + chrono::Days::new(2);
        let second_date = today + chrono::Days::new(7);
        let end_date = second_date.succ_opt().unwrap();
        tokio::fs::write(
            &first,
            format!(concat!(
                "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:first\nSUMMARY:First\nDTSTART:{date}T090000Z\nDTEND:{date}T100000Z\nEND:VEVENT\n",
                "BEGIN:VEVENT\nUID:expired\nSUMMARY:Expired\nDTSTART:{expired_start}\nDTEND:{expired_end}\nEND:VEVENT\n",
                "BEGIN:VEVENT\nUID:ongoing\nSUMMARY:Ongoing\nDTSTART:{ongoing_start}\nDTEND:{ongoing_end}\nEND:VEVENT\nEND:VCALENDAR\n"
            ), date = first_date.format("%Y%m%d"),
               expired_start = (now - chrono::Duration::hours(2)).format("%Y%m%dT%H%M%SZ"),
               expired_end = (now - chrono::Duration::hours(1)).format("%Y%m%dT%H%M%SZ"),
               ongoing_start = (now - chrono::Duration::minutes(30)).format("%Y%m%dT%H%M%SZ"),
               ongoing_end = (now + chrono::Duration::hours(1)).format("%Y%m%dT%H%M%SZ")),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &second,
            format!("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:second\nSUMMARY:Second\nDTSTART;VALUE=DATE:{}\nDTEND;VALUE=DATE:{}\nEND:VEVENT\nEND:VCALENDAR\n", second_date.format("%Y%m%d"), end_date.format("%Y%m%d")),
        )
        .await
        .unwrap();
        let config_path = directory.path().join("activity.json");
        tokio::fs::write(
            &config_path,
            format!(
                r##"{{"calendar_sources":[{{"id":"one","kind":"ics-file","path":"{}"}},{{"id":"two","kind":"ics-file","path":"{}"}}],"world_clocks":[{{"timezone":"Etc/UTC"}},{{"timezone":"Etc/UTC","label":"Home"}}]}}"##,
                first.display(),
                second.display()
            ),
        )
        .await
        .unwrap();
        let state = StateStore::default();
        let service = ActivityService::with_paths(
            state.clone(),
            config_path,
            directory.path().join("todos.json"),
        )
        .await;
        service.refresh().await;
        let snapshot = state.snapshot().await.activity;
        assert_eq!(snapshot.event_count, 4);
        assert_eq!(snapshot.world_clocks.len(), 2);
        assert_eq!(snapshot.world_clocks[0].city, "UTC");
        assert_eq!(snapshot.world_clocks[0].label, "UTC");
        assert_eq!(snapshot.world_clocks[0].abbreviation, "UTC");
        assert_eq!(snapshot.world_clocks[0].utc_offset_seconds, 0);
        assert_eq!(snapshot.world_clocks[1].label, "Home");
        assert_eq!(snapshot.next_event.as_ref().unwrap().title, "Ongoing");
        assert!(
            snapshot.lunar.is_some(),
            "lunar metadata does not require weather"
        );
        assert!(!snapshot.weather.available);
        assert!(snapshot.weather.solar_noon.is_none());
        assert_eq!(snapshot.sources.len(), 2);
        assert!(snapshot.sources.iter().all(|source| source.available));
        let range = service
            .query_range_in_timezone(
                first_date
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis(),
                end_date
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis(),
                chrono_tz::UTC,
            )
            .await
            .unwrap();
        assert_eq!(range.events.len(), 2);
        assert_eq!(
            range.busy_dates,
            vec![first_date.to_string(), second_date.to_string()]
        );

        // A partial write must retain the source's last good data, report its
        // failure independently, and recover when the file is valid again.
        tokio::fs::write(&first, "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:first\n")
            .await
            .unwrap();
        service.refresh().await;
        let failed = state.snapshot().await.activity;
        assert_eq!(failed.event_count, 4);
        assert!(
            failed.lunar.is_some(),
            "provider failures do not remove lunar metadata"
        );
        assert!(!failed.sources[0].available);
        assert!(failed.sources[0].error.is_some());
        assert!(failed.sources[1].available);
        assert_eq!(
            service
                .query_range(range.from_unix_ms, range.to_unix_ms)
                .await
                .unwrap()
                .events,
            range.events
        );

        tokio::fs::write(&first, "BEGIN:VCALENDAR\nEND:VCALENDAR\n")
            .await
            .unwrap();
        service.refresh().await;
        let recovered = state.snapshot().await.activity;
        assert_eq!(recovered.event_count, 1);
        assert!(recovered.sources[0].available);
        assert!(recovered.error.is_none());
    }
}
