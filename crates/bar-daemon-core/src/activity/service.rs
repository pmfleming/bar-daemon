use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{Local, TimeZone};
use tokio::sync::{Mutex, RwLock};

use crate::{state::StateStore, time::unix_ms_i64 as unix_ms};

#[path = "agenda_service.rs"]
mod agenda_service;

use super::{
    config::{self, ActivityConfig},
    model::{
        ActivityEvent, ActivityRange, ActivitySourceState, ActivityState, WeatherState,
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
    // One accepted record snapshot; legacy task counts are derived, not mirrored.
    // Turso remains the single writable persistence owner.
    agenda_store: Option<super::agenda_store::Store>,
    agenda_snapshot: Option<super::agenda_store::model::Snapshot>,
    import_revision: u64,
    agenda_catalog: Option<super::agenda::CachedCatalog>,
    todo_error: Option<String>,
    weather: WeatherState,
    weather_locations: Vec<WeatherState>,
    catalog: Option<super::catalog::Catalog>,
    catalog_error: Option<String>,
}

impl ActivityData {
    fn readable_agenda(&self) -> Result<&super::agenda_store::model::Snapshot> {
        if let Some(error) = &self.todo_error {
            bail!(super::agenda::bounded_diagnostic(error, 1024));
        }
        self.agenda_snapshot
            .as_ref()
            .context("Agenda store unavailable; no accepted snapshot")
    }

    fn source_diagnostics(&self) -> super::agenda::SourceDiagnostics {
        super::agenda::source_diagnostics(self.config.calendar_sources.iter().filter_map(
            |source| {
                let error = self.source_states.get(&source.id)?.error.as_deref()?;
                Some((source.display_name(), error))
            },
        ))
    }

    fn incomplete_todo_count(&self) -> u32 {
        self.agenda_snapshot
            .as_ref()
            .map_or(0, |snapshot| {
                snapshot
                    .items
                    .iter()
                    .filter(|item| item.task.as_ref().is_some_and(|task| !task.completed))
                    .count()
            })
            .try_into()
            .unwrap_or(u32::MAX)
    }

    // Apply a committed contiguous delta. The service reloads a full snapshot
    // on revision gaps; historical receipts must never roll accepted data back.
    fn apply_agenda_result(
        &mut self,
        result: &super::agenda_store::model::MutationResult,
    ) -> Result<bool> {
        let snapshot = self
            .agenda_snapshot
            .as_mut()
            .context("Agenda cache unavailable; resolve request by receipt")?;
        if result.catalog_revision <= snapshot.catalog_revision {
            return Ok(false);
        }
        if let Some(item) = &result.item {
            snapshot.items.retain(|record| record.id != item.id);
            snapshot.items.push(item.clone());
        }
        if let Some(id) = &result.deleted_id {
            snapshot.items.retain(|record| &record.id != id);
        }
        snapshot.catalog_revision = result.catalog_revision;
        Ok(true)
    }

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
        let opened = Self::open_agenda(&todo_path).await;
        let (agenda_store, agenda_snapshot, todo_error) = match opened {
            Ok((store, snapshot)) => (Some(store), Some(snapshot), None),
            Err(error) => (None, None, Some(super::agenda::store_unavailable(&error))),
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
                agenda_store,
                agenda_snapshot,
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

    async fn open_agenda(
        path: &std::path::Path,
    ) -> Result<(
        super::agenda_store::Store,
        super::agenda_store::model::Snapshot,
    )> {
        let store =
            super::agenda_store::Store::open(path.with_file_name("agenda"), Some(path.to_owned()))
                .await?;
        let snapshot = store.snapshot().await?;
        Ok((store, snapshot))
    }

    // Caller holds todo_guard. Only failed startup is retried, never an
    // uncertain mutation, and successful owners are never silently reopened.
    async fn writable_agenda(&self) -> Result<super::agenda_store::Store> {
        if let Some(store) = &self.data.read().await.agenda_store {
            return Ok(store.clone());
        }
        match Self::open_agenda(&self.todo_path).await {
            Ok((store, snapshot)) => {
                let mut data = self.data.write().await;
                data.agenda_snapshot = Some(snapshot);
                data.agenda_store = Some(store.clone());
                data.todo_error = None;
                Ok(store)
            }
            Err(error) => {
                let message = super::agenda::store_unavailable(&error);
                self.data.write().await.todo_error = Some(message.clone());
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
        let mut clock_check = tokio::time::interval(Duration::from_secs(1));
        clock_check.tick().await;
        let mut previous = (chrono::Utc::now(), std::time::Instant::now());
        loop {
            tokio::select! {
                _ = tokio::time::sleep(super::presentation::next_minute(chrono::Utc::now())) => self.publish_clock(chrono::Utc::now()).await,
                _ = clock_check.tick() => {
                    let now = chrono::Utc::now();
                    if clock_shifted(previous.0,previous.1.elapsed(),now) { self.publish_clock(now).await; }
                    previous = (now,std::time::Instant::now());
                }
            }
        }
    }

    async fn publish_clock(&self, now: chrono::DateTime<chrono::Utc>) {
        // Independent of provider I/O; publication shares the existing guard.
        let _publication = self.publication_guard.lock().await;
        let mut state = self.state.read(|s| s.activity.clone()).await;
        super::presentation::update(&mut state, now);
        state.agenda = self.agenda_info_at(now).await;
        self.state.update_activity(state).await;
    }

    async fn monitor_sources(self: Arc<Self>) {
        let mut events = self.state.subscribe_domains(&[
            crate::protocol::stream::TIMEZONE,
            crate::protocol::stream::POWER_SLEEP,
        ]);
        self.refresh().await;
        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await;
        loop {
            tokio::select! {
                _ = refresh.tick() => self.refresh().await,
                event = events.recv() => match event {
                    Ok(event) if event.stream == crate::protocol::stream::TIMEZONE => self.publish_state(None).await,
                    Ok(event) if event.stream == crate::protocol::stream::POWER_SLEEP => self.publish_clock(chrono::Utc::now()).await,
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
            if needs_recovery && let Err(error) = self.writable_agenda().await {
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
        let (mut imports, mut source_states) = {
            let data = self.data.read().await;
            (data.events_by_source.clone(), data.source_states.clone())
        };
        imports.retain(|id, _| configured_ids.contains(id.as_str()));
        source_states.retain(|id, _| configured_ids.contains(id.as_str()));
        let mut first_error = None;
        let calendar_results = futures::future::join_all(
            config
                .calendar_sources
                .iter()
                .map(|source| async move { (source, self.providers.load(source).await) }),
        )
        .await;
        for (source, result) in calendar_results {
            let error = match result {
                Ok(events) => {
                    imports.insert(source.id.clone(), events);
                    None
                }
                Err(error) => Some(super::agenda::source_error_cause(&format!("{error:#}")).into()),
            };
            first_error = first_error.or_else(|| error.clone());
            let item_count = imports
                .get(&source.id)
                .map_or(0, Vec::len)
                .try_into()
                .unwrap_or(u32::MAX);
            source_states.insert(
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
        if let Err(error) = self.accept_calendar_imports(imports).await {
            let message =
                format!("Calendar acceptance failed; retained previous import: {error:#}");
            first_error = Some(message.clone());
            let accepted = self.data.read().await;
            for source in source_states.values_mut() {
                source.available = false;
                source.item_count = accepted
                    .events_by_source
                    .get(&source.id)
                    .map_or(0, Vec::len)
                    .try_into()
                    .unwrap_or(u32::MAX);
                source.error = Some(message.clone());
            }
        }
        {
            let mut data = self.data.write().await;
            // Saved cities remain independently owned; calendar refresh never rewrites config.
            data.config = config.clone();
            data.source_states = source_states;
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
        let snapshot = data.readable_agenda()?;
        let imported: HashMap<_, _> = data
            .events_by_source
            .values()
            .flatten()
            .map(|event| (super::agenda_adapter::imported_id(event), event))
            .collect();
        let mut items = snapshot.items.clone();
        items.extend(
            imported
                .values()
                .map(|event| super::agenda_adapter::imported(event))
                .collect::<Result<Vec<_>>>()?,
        );
        let today = chrono::Utc::now().with_timezone(&timezone).date_naive();
        let projection = super::agenda_projection::Projection {
            today,
            zone: timezone,
        };
        let catalog = super::agenda::Catalog::new(items, &projection);
        let items = &catalog.items;
        let mut days = std::collections::BTreeMap::new();
        let mut ids = BTreeSet::new();
        for day in std::iter::successors(Some(range.first), |d| d.succ_opt())
            .take_while(|d| *d <= range.last)
        {
            let mut membership = super::model::ActivityDay::default();
            for item in catalog.on_day(day, projection.bounds(day)) {
                ids.insert(item.id.clone());
                match item.kind {
                    super::agenda_store::model::ItemKind::Event => membership.event_ids.push(
                        imported
                            .get(&item.id)
                            .map_or_else(|| item.id.clone(), |event| event.id.clone()),
                    ),
                    super::agenda_store::model::ItemKind::Task => {
                        membership.todo_ids.push(item.id.clone())
                    }
                }
            }
            membership.event_ids.sort();
            membership.todo_ids.sort();
            days.insert(day.to_string(), membership);
        }
        let mut events = items
            .iter()
            .filter(|i| ids.contains(&i.id))
            .filter_map(|i| {
                if let Some(event) = imported.get(&i.id) {
                    Some((*event).clone())
                } else {
                    super::agenda_adapter::legacy_event(i, &projection)
                }
            })
            .collect::<Vec<_>>();
        events.sort_by(ActivityEvent::chronological_cmp);
        let todos = items
            .iter()
            .filter(|i| ids.contains(&i.id))
            .filter_map(super::agenda_adapter::todo)
            .collect::<Vec<_>>();
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

    async fn publish_state(&self, error: Option<String>) {
        let _publication = self.publication_guard.lock().await;
        let agenda = self.agenda_info().await;
        let data = self.data.read().await;
        let now = unix_ms();
        let events = data.events_by_source.values().flatten();
        let mut next_event = events
            .clone()
            .filter(|event| event.end_unix_ms >= now)
            .min_by_key(|event| event.start_unix_ms)
            .cloned();
        let local_event_count = data.agenda_snapshot.as_ref().map_or(0, |s| {
            s.items
                .iter()
                .filter(|i| i.kind == super::agenda_store::model::ItemKind::Event)
                .count()
        });
        let event_count = events.count().saturating_add(local_event_count);
        if let Some(info) = &agenda
            && let (Ok(today), Ok(zone)) = (
                super::agenda_projection::date(&info.today),
                info.timezone.parse::<chrono_tz::Tz>(),
            )
            && let Some(snapshot) = &data.agenda_snapshot
        {
            let p = super::agenda_projection::Projection { today, zone };
            let local_next = snapshot
                .items
                .iter()
                .filter(|item| {
                    item.kind == super::agenda_store::model::ItemKind::Event
                        && p.membership(&item.schedule).is_some()
                })
                .filter_map(|item| super::agenda_adapter::legacy_event(item, &p))
                .filter(|event| event.end_unix_ms >= now)
                .min_by(ActivityEvent::chronological_cmp);
            next_event = next_event
                .into_iter()
                .chain(local_next)
                .min_by(ActivityEvent::chronological_cmp);
        }
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
            event_count: event_count.try_into().unwrap_or(u32::MAX),
            incomplete_todo_count: data.incomplete_todo_count(),
            next_event,
            agenda,
            sources,
            world_clocks,
            locations: Vec::new(),
            location_catalog: catalog.as_ref().map(|c| super::catalog::CatalogInfo {
                revision: c.revision,
                writable: data.catalog_error.is_none(),
            }),
            lunar: super::astronomy::lunar(now),
            // Presentation projects astronomy and labels once, in place, after routing.
            weather: data.weather.clone(),
            weather_locations: data.weather_locations.clone(),
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

// Compare civil wall time with monotonic elapsed time. A clock step (or resume
// on systems whose monotonic clock pauses in sleep) must invalidate open views,
// not merely wait for the next pre-step minute timer. No persisted dates change.
fn clock_shifted(
    previous: chrono::DateTime<chrono::Utc>,
    elapsed: Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Ok(elapsed) = chrono::Duration::from_std(elapsed) else {
        return true;
    };
    let Some(expected) = previous.checked_add_signed(elapsed) else {
        return true;
    };
    (now - expected).num_milliseconds().unsigned_abs() > 250
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
    #[test]
    fn clock_step_detection_ignores_elapsed_time_but_catches_forward_and_backward_steps() {
        let previous = chrono::DateTime::parse_from_rfc3339("2026-10-27T23:59:59Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let elapsed = std::time::Duration::from_secs(2);
        assert!(!super::clock_shifted(
            previous,
            elapsed,
            previous + chrono::Duration::seconds(2)
        ));
        assert!(super::clock_shifted(
            previous,
            elapsed,
            previous + chrono::Duration::days(1)
        ));
        assert!(super::clock_shifted(
            previous,
            elapsed,
            previous - chrono::Duration::hours(1)
        ));
    }
    #[tokio::test]
    async fn actual_resume_stream_republishes_projection_without_provider_or_store_writes() {
        let directory = tempdir().unwrap();
        let service = service_in(&directory).await;
        service
            .create_todo("Keep endpoint".into(), None, Some("2030-10-30".into()), 0)
            .await
            .unwrap();
        let mut events = service
            .state
            .subscribe_domains(&[crate::protocol::stream::ACTIVITY]);
        let monitor = tokio::spawn(std::sync::Arc::clone(&service).monitor_sources());
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                events.recv().await.unwrap();
                if !service.state.read(|s| s.activity.syncing).await {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let before = service.data_snapshot_for_test().await;
        let mut stale = service.state.read(|s| s.activity.clone()).await;
        stale.agenda.as_mut().unwrap().today = "1900-01-01".into();
        service.state.update_activity(stale).await;
        service.state.record_resume().await;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                events.recv().await.unwrap();
                if service
                    .state
                    .read(|s| s.activity.agenda.as_ref().unwrap().today != "1900-01-01")
                    .await
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let after = service.data_snapshot_for_test().await;
        assert_eq!(before.catalog_revision, after.catalog_revision);
        assert_eq!(before.items, after.items);
        monitor.abort();
        let _ = monitor.await;
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn explicit_receipt_reconciles_committed_but_unpublished_outcome_and_cache_gaps() {
        let directory = tempdir().unwrap();
        let service = service_in(&directory).await;
        let store = service.data.read().await.agenda_store.clone().unwrap();
        let item = |title| {
            serde_json::from_value(serde_json::json!({"kind":"task","title":title})).unwrap()
        };
        // Simulate a completion lost after durable worker commit, before service
        // publication. Receipt recovery is a read, never mutation replay.
        let committed = store
            .create("unpublished".into(), item("Recovered"))
            .await
            .unwrap();
        assert!(
            service
                .data
                .read()
                .await
                .agenda_snapshot
                .as_ref()
                .unwrap()
                .items
                .is_empty()
        );
        let receipt = service.agenda_receipt("unpublished".into()).await.unwrap();
        assert_eq!(
            receipt["receipt"],
            serde_json::to_value(&committed).unwrap()
        );
        assert_eq!(
            service
                .data
                .read()
                .await
                .agenda_snapshot
                .as_ref()
                .unwrap()
                .items
                .len(),
            1
        );
        assert_eq!(
            service
                .state
                .read(|s| s.activity.incomplete_todo_count)
                .await,
            1
        );
        let second = store
            .create("second-unpublished".into(), item("Also recovered"))
            .await
            .unwrap();
        let third = service
            .agenda_create("third".into(), item("Normal mutation"))
            .await
            .unwrap();
        assert!(third.catalog_revision > second.catalog_revision);
        assert_eq!(
            service
                .data
                .read()
                .await
                .agenda_snapshot
                .as_ref()
                .unwrap()
                .items
                .len(),
            3,
            "revision gap reloads complete accepted snapshot"
        );
        assert_eq!(
            service
                .state
                .read(|s| s.activity.incomplete_todo_count)
                .await,
            3
        );
        let completed_id = committed.item.as_ref().unwrap().id.clone();
        service.complete_todo(&completed_id, true).await.unwrap();
        assert_eq!(
            service
                .state
                .read(|s| s.activity.incomplete_todo_count)
                .await,
            2
        );
        let accepted = service.data.read().await.agenda_snapshot.clone().unwrap();
        let historical = service.agenda_receipt("unpublished".into()).await.unwrap();
        assert_eq!(
            historical["receipt"],
            serde_json::to_value(&committed).unwrap()
        );
        let current = service.data.read().await.agenda_snapshot.clone().unwrap();
        assert_eq!(
            current.items, accepted.items,
            "old receipts cannot roll back completion or other records"
        );
        assert_eq!(current.catalog_revision, accepted.catalog_revision);
        assert_eq!(
            service
                .state
                .read(|s| s.activity.incomplete_todo_count)
                .await,
            2
        );
        service.delete_todo(&second.item.unwrap().id).await.unwrap();
        assert_eq!(
            service
                .state
                .read(|s| s.activity.incomplete_todo_count)
                .await,
            1
        );
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn midnight_timezone_and_resume_refresh_projection_without_stored_endpoint_writes() {
        use chrono::TimeZone;
        let directory = tempdir().unwrap();
        let service = service_in(&directory).await;
        service
            .state
            .update_timezone(crate::model::TimezoneState {
                timezone: "UTC".into(),
                ..Default::default()
            })
            .await;
        service
            .create_todo("End only".into(), None, Some("2026-10-30".into()), 0)
            .await
            .unwrap();
        let before = service.data_snapshot_for_test().await;
        assert!(before.items[0].schedule.start_date.is_none());
        service
            .publish_clock(
                chrono::Utc
                    .with_ymd_and_hms(2026, 10, 27, 23, 59, 0)
                    .unwrap(),
            )
            .await;
        assert_eq!(
            service
                .state
                .read(|s| s.activity.agenda.as_ref().unwrap().today.clone())
                .await,
            "2026-10-27"
        );
        service
            .publish_clock(chrono::Utc.with_ymd_and_hms(2026, 10, 28, 0, 0, 0).unwrap())
            .await;
        assert_eq!(
            service
                .state
                .read(|s| s.activity.agenda.as_ref().unwrap().today.clone())
                .await,
            "2026-10-28"
        );
        service
            .state
            .update_timezone(crate::model::TimezoneState {
                timezone: "Asia/Tokyo".into(),
                ..Default::default()
            })
            .await;
        service
            .publish_clock(
                chrono::Utc
                    .with_ymd_and_hms(2026, 10, 28, 23, 30, 0)
                    .unwrap(),
            )
            .await;
        let info = service
            .state
            .read(|s| s.activity.agenda.clone().unwrap())
            .await;
        assert_eq!(info.today, "2026-10-29");
        assert_eq!(info.timezone, "Asia/Tokyo");
        let after = service.data_snapshot_for_test().await;
        assert_eq!(after.catalog_revision, before.catalog_revision);
        assert_eq!(after.items, before.items);
        service.shutdown().await.unwrap();
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
            let store = service.data.read().await.agenda_store.clone().unwrap();
            let stored = store.snapshot().await.unwrap();
            assert_eq!(stored.items.len(), 2);
            assert!(stored.items.iter().any(|i| i.id == "recoverable-task"));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                recovered.to_string()
            );
            assert!(state.snapshot().await.activity.error.is_none());

            // A failed database owner never publishes an unconfirmed mutation.
            let before = state.snapshot().await.activity;
            let accepted = service.data.read().await.agenda_snapshot.clone().unwrap();
            store.shutdown().await.unwrap();
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
            let current = service.data.read().await.agenda_snapshot.clone().unwrap();
            assert_eq!(current.items, accepted.items);
            assert_eq!(current.catalog_revision, accepted.catalog_revision);
            assert_eq!(state.snapshot().await.activity, before);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                recovered.to_string()
            );
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
        service
            .data
            .read()
            .await
            .agenda_store
            .as_ref()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        drop(service);
        let service = ActivityService::with_paths(StateStore::default(), config, todos).await;
        // Unscheduled records persist but are list-only, never a calendar day.
        assert!(service.query_range(0, 1).await.unwrap().todos.is_empty());
        assert_eq!(
            service
                .data
                .read()
                .await
                .agenda_snapshot
                .as_ref()
                .unwrap()
                .items
                .iter()
                .filter_map(crate::activity::agenda_adapter::todo)
                .collect::<Vec<_>>(),
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
                        .agenda_create(
                            format!("day-{zone}-{day}"),
                            serde_json::from_value(serde_json::json!({
                                "kind":"task", "title":day,
                                "schedule":{"mode":"date","start_date":day,"end_date":day}
                            }))
                            .unwrap(),
                        )
                        .await
                        .unwrap()
                        .item
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
