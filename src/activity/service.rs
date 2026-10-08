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
}

pub(crate) struct ActivityService {
    data: RwLock<ActivityData>,
    state: StateStore,
    config_path: PathBuf,
    todo_path: PathBuf,
    providers: ProviderRegistry,
    refresh_guard: Mutex<()>,
    todo_guard: Mutex<()>,
    todo_sequence: AtomicU64,
}

impl ActivityService {
    pub(crate) async fn new(state: StateStore) -> Arc<Self> {
        Self::with_paths(state, config::config_path(), config::todo_path()).await
    }

    async fn with_paths(state: StateStore, config_path: PathBuf, todo_path: PathBuf) -> Arc<Self> {
        let (todos, todo_error) = match config::load_todos(&todo_path).await {
            Ok(todos) => (todos, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Local todo store is unavailable: {error:#}")),
            ),
        };
        let service = Arc::new(Self {
            data: RwLock::new(ActivityData {
                todos,
                todo_error,
                ..ActivityData::default()
            }),
            state,
            config_path,
            todo_path,
            providers: ProviderRegistry::builtins(),
            refresh_guard: Mutex::new(()),
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
        let mut events = self.state.subscribe();
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
            if data.config.configured_weather_locations() != config.configured_weather_locations() {
                data.weather_locations.clear();
            }
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
        let locations = config.configured_weather_locations();
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
        let cached = self.data.read().await.weather_locations.clone();
        let now = unix_ms();
        let forecasts = futures::future::join_all(locations.iter().map(|location| {
            let previous = cached.iter().find(|weather| weather.id == location.id);
            async move {
                if let Some(weather) = previous
                    && weather.available
                    && now.saturating_sub(weather.updated_unix_ms) < 15 * 60 * 1_000
                {
                    return weather.clone();
                }
                match weather::fetch(location).await {
                    Ok(forecast) => forecast,
                    Err(error) => WeatherState {
                        id: location.id.clone(),
                        location: location.location.clone(),
                        home: location.home,
                        timezone: location.timezone.clone(),
                        error: Some(error.to_string()),
                        ..previous.cloned().unwrap_or_default()
                    },
                }
            }
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
        if to_unix_ms <= from_unix_ms {
            bail!("to_unix_ms must be greater than from_unix_ms");
        }
        const MAX_RANGE_MS: i64 = 370 * 24 * 60 * 60 * 1_000;
        let duration = to_unix_ms
            .checked_sub(from_unix_ms)
            .context("activity range duration overflows")?;
        if duration > MAX_RANGE_MS {
            bail!("activity range cannot exceed 370 days");
        }
        let data = self.data.read().await;
        let mut events = data
            .events_by_source
            .values()
            .flatten()
            .filter(|event| event.end_unix_ms > from_unix_ms && event.start_unix_ms < to_unix_ms)
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(|a, b| {
            (a.start_unix_ms, a.end_unix_ms, &a.id).cmp(&(b.start_unix_ms, b.end_unix_ms, &b.id))
        });
        // Date-only todos occupy a local calendar day. Include every local date
        // touched by the half-open interval, including partial days and DST.
        let from_date = timezone
            .timestamp_millis_opt(from_unix_ms)
            .single()
            .context("activity range start is outside supported dates")?
            .date_naive()
            .to_string();
        let last_date = timezone
            .timestamp_millis_opt(to_unix_ms - 1)
            .single()
            .context("activity range end is outside supported dates")?
            .date_naive()
            .to_string();
        let mut todos = data
            .todos
            .iter()
            .filter(|todo| match (todo.due_unix_ms, todo.due_date.as_deref()) {
                (Some(due), _) => due >= from_unix_ms && due < to_unix_ms,
                (None, Some(date)) => date >= from_date.as_str() && date <= last_date.as_str(),
                (None, None) => true,
            })
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
        let days = super::day::project(
            &events,
            &todos,
            from_date.parse()?,
            last_date.parse()?,
            today,
            &timezone,
        );
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
        let _guard = self.todo_guard.lock().await;
        let mut todos = self.writable_todos().await?;
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
        config::save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None).await;
        Ok(todo)
    }

    pub(crate) async fn complete_todo(&self, id: &str, completed: bool) -> Result<TodoItem> {
        let _guard = self.todo_guard.lock().await;
        let mut todos = self.writable_todos().await?;
        let todo = todos
            .iter_mut()
            .find(|todo| todo.id == id)
            .with_context(|| format!("todo {id} was not found"))?;
        todo.completed = completed;
        todo.completed_unix_ms = completed.then(unix_ms);
        let result = todo.clone();
        config::save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None).await;
        Ok(result)
    }

    pub(crate) async fn delete_todo(&self, id: &str) -> Result<()> {
        let _guard = self.todo_guard.lock().await;
        let mut todos = self.writable_todos().await?;
        let previous = todos.len();
        todos.retain(|todo| todo.id != id);
        if todos.len() == previous {
            bail!("todo {id} was not found");
        }
        config::save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None).await;
        Ok(())
    }

    async fn publish_state(&self, error: Option<String>) {
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
        let error = data.todo_error.clone().or(error).or_else(|| {
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
        state.locations = super::locations::project(&state, &local);
        self.state.update_activity(state).await;
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::state::StateStore;

    use super::ActivityService;

    #[tokio::test]
    async fn failed_todo_load_blocks_all_mutations_and_recovers_without_data_loss() {
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
            let state = StateStore::default();
            let service = ActivityService::with_paths(
                state.clone(),
                directory.path().join("activity.json"),
                path.clone(),
            )
            .await;
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
