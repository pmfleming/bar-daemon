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
use chrono::{Local, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use tokio::sync::{Mutex, RwLock};

use crate::{
    activity::notifications::service::NotificationSink, state::StateStore,
    time::unix_ms_i64 as unix_ms,
};

use super::{
    config::{self, ActivityConfig},
    model::{
        ActivityEvent, ActivityRange, ActivitySourceState, ActivityState, TodoItem, WorldClockState,
    },
    provider::ProviderRegistry,
};

#[derive(Default)]
struct ActivityData {
    config: ActivityConfig,
    events_by_source: HashMap<String, Vec<ActivityEvent>>,
    source_states: HashMap<String, ActivitySourceState>,
    todos: Vec<TodoItem>,
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
    _notifications: NotificationSink,
}

impl ActivityService {
    pub(crate) async fn new(state: StateStore, notifications: NotificationSink) -> Arc<Self> {
        let todo_path = config::todo_path();
        let todos = load_todos(&todo_path).await.unwrap_or_else(|error| {
            tracing::warn!(%error, "local todo store could not be loaded");
            Vec::new()
        });
        Arc::new(Self {
            data: RwLock::new(ActivityData {
                todos,
                ..ActivityData::default()
            }),
            state,
            config_path: config::config_path(),
            todo_path,
            providers: ProviderRegistry::builtins(),
            refresh_guard: Mutex::new(()),
            todo_guard: Mutex::new(()),
            todo_sequence: AtomicU64::new(1),
            _notifications: notifications,
        })
    }

    pub(crate) async fn monitor(self: Arc<Self>) {
        self.refresh().await;
        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await;
        loop {
            refresh.tick().await;
            self.refresh().await;
        }
    }

    pub(crate) async fn refresh(&self) {
        let Ok(_guard) = self.refresh_guard.try_lock() else {
            return;
        };
        let current = self.state.snapshot().await.activity;
        self.state
            .update_activity(ActivityState {
                available: true,
                syncing: true,
                ..current
            })
            .await;

        let config = match config::load(&self.config_path).await {
            Ok(config) if config::validate(&config).is_ok() => config,
            Ok(config) => {
                let error = config::validate(&config).unwrap_err();
                self.publish_state(Some(error.to_string()), false).await;
                return;
            }
            Err(error) => {
                self.publish_state(Some(error.to_string()), false).await;
                return;
            }
        };

        let configured_ids = config
            .calendar_sources
            .iter()
            .map(|source| source.id.clone())
            .collect::<BTreeSet<_>>();
        {
            let mut data = self.data.write().await;
            data.config = config.clone();
            data.events_by_source
                .retain(|source_id, _| configured_ids.contains(source_id));
            data.source_states
                .retain(|source_id, _| configured_ids.contains(source_id));
        }

        let mut first_error = None;
        for source in &config.calendar_sources {
            let result = self.providers.load(source).await;
            let mut data = self.data.write().await;
            match result {
                Ok(events) => {
                    data.source_states.insert(
                        source.id.clone(),
                        ActivitySourceState {
                            id: source.id.clone(),
                            name: source_name(source),
                            kind: source.kind.clone(),
                            available: true,
                            item_count: events.len().try_into().unwrap_or(u32::MAX),
                            error: None,
                        },
                    );
                    data.events_by_source.insert(source.id.clone(), events);
                }
                Err(error) => {
                    let message = error.to_string();
                    first_error.get_or_insert_with(|| message.clone());
                    let retained_count = data
                        .events_by_source
                        .get(&source.id)
                        .map_or(0, Vec::len)
                        .try_into()
                        .unwrap_or(u32::MAX);
                    data.source_states.insert(
                        source.id.clone(),
                        ActivitySourceState {
                            id: source.id.clone(),
                            name: source_name(source),
                            kind: source.kind.clone(),
                            available: false,
                            item_count: retained_count,
                            error: Some(message),
                        },
                    );
                }
            }
        }
        self.publish_state(first_error, false).await;
    }

    pub(crate) async fn query_range(
        &self,
        from_unix_ms: i64,
        to_unix_ms: i64,
    ) -> Result<ActivityRange> {
        if to_unix_ms <= from_unix_ms {
            bail!("to_unix_ms must be greater than from_unix_ms");
        }
        const MAX_RANGE_MS: i64 = 370 * 24 * 60 * 60 * 1_000;
        if to_unix_ms - from_unix_ms > MAX_RANGE_MS {
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
        events.sort_by_key(|event| (event.start_unix_ms, event.end_unix_ms, event.id.clone()));
        let from_date = Utc
            .timestamp_millis_opt(from_unix_ms)
            .single()
            .map(|value| value.format("%Y-%m-%d").to_string());
        let to_date = Utc
            .timestamp_millis_opt(to_unix_ms)
            .single()
            .map(|value| value.format("%Y-%m-%d").to_string());
        let mut todos = data
            .todos
            .iter()
            .filter(|todo| match (todo.due_unix_ms, todo.due_date.as_deref()) {
                (Some(due), _) => due >= from_unix_ms && due < to_unix_ms,
                (None, Some(date)) => {
                    from_date.as_deref().is_none_or(|from| date >= from)
                        && to_date.as_deref().is_none_or(|to| date < to)
                }
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
        let busy_dates = events
            .iter()
            .filter_map(event_date)
            .chain(todos.iter().filter_map(|todo| todo.due_date.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(ActivityRange {
            from_unix_ms,
            to_unix_ms,
            events,
            todos,
            busy_dates,
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
        let mut todos = self.data.read().await.todos.clone();
        todos.push(todo.clone());
        save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None, false).await;
        Ok(todo)
    }

    pub(crate) async fn complete_todo(&self, id: &str, completed: bool) -> Result<TodoItem> {
        let _guard = self.todo_guard.lock().await;
        let mut todos = self.data.read().await.todos.clone();
        let todo = todos
            .iter_mut()
            .find(|todo| todo.id == id)
            .with_context(|| format!("todo {id} was not found"))?;
        todo.completed = completed;
        todo.completed_unix_ms = completed.then(unix_ms);
        let result = todo.clone();
        save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None, false).await;
        Ok(result)
    }

    pub(crate) async fn delete_todo(&self, id: &str) -> Result<()> {
        let _guard = self.todo_guard.lock().await;
        let mut todos = self.data.read().await.todos.clone();
        let previous = todos.len();
        todos.retain(|todo| todo.id != id);
        if todos.len() == previous {
            bail!("todo {id} was not found");
        }
        save_todos(&self.todo_path, &todos).await?;
        self.data.write().await.todos = todos;
        self.publish_state(None, false).await;
        Ok(())
    }

    async fn publish_state(&self, error: Option<String>, syncing: bool) {
        let data = self.data.read().await;
        let now = unix_ms();
        let mut events = data
            .events_by_source
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.start_unix_ms);
        let next_event = events
            .iter()
            .find(|event| event.end_unix_ms >= now)
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
                        name: source_name(source),
                        kind: source.kind.clone(),
                        ..ActivitySourceState::default()
                    })
            })
            .collect();
        let world_clocks = data
            .config
            .world_clocks
            .iter()
            .filter_map(|clock| world_clock(&clock.timezone, &clock.label).ok())
            .collect();
        let error = error.or_else(|| {
            sources
                .iter()
                .find_map(|source: &ActivitySourceState| source.error.clone())
        });
        let state = ActivityState {
            available: true,
            syncing,
            event_count: events.len().try_into().unwrap_or(u32::MAX),
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
            error,
        };
        drop(data);
        self.state.update_activity(state).await;
    }
}

fn source_name(source: &super::config::CalendarSourceConfig) -> String {
    if source.name.is_empty() {
        source.id.clone()
    } else {
        source.name.clone()
    }
}

fn world_clock(timezone: &str, label: &str) -> Result<WorldClockState> {
    let zone: Tz = timezone
        .parse()
        .with_context(|| format!("parse world-clock timezone {timezone}"))?;
    let now = Utc::now().with_timezone(&zone);
    let city = timezone
        .rsplit('/')
        .next()
        .unwrap_or(timezone)
        .replace('_', " ");
    Ok(WorldClockState {
        timezone: timezone.into(),
        label: if label.is_empty() {
            city.clone()
        } else {
            label.into()
        },
        city,
        abbreviation: now.format("%Z").to_string(),
        utc_offset_seconds: now.offset().fix().local_minus_utc(),
    })
}

fn event_date(event: &ActivityEvent) -> Option<String> {
    event.start_date.clone().or_else(|| {
        Local
            .timestamp_millis_opt(event.start_unix_ms)
            .single()
            .map(|date| date.format("%Y-%m-%d").to_string())
    })
}

async fn load_todos(path: &PathBuf) -> Result<Vec<TodoItem>> {
    match tokio::fs::read(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .with_context(|| format!("parse todo store {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

async fn save_todos(path: &PathBuf, todos: &[TodoItem]) -> Result<()> {
    let parent = path.parent().context("todo store path has no parent")?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("create {}", parent.display()))?;
    let temporary = path.with_extension("json.tmp");
    let contents = serde_json::to_vec_pretty(todos)?;
    tokio::fs::write(&temporary, contents)
        .await
        .with_context(|| format!("write {}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use tempfile::tempdir;

    use crate::{activity::notifications::service::NotificationSink, state::StateStore};

    use super::{ActivityService, ProviderRegistry};

    #[tokio::test]
    async fn local_todos_round_trip_and_query() {
        let directory = tempdir().unwrap();
        let service = ActivityService {
            data: Default::default(),
            state: StateStore::default(),
            config_path: directory.path().join("activity.json"),
            todo_path: directory.path().join("todos.json"),
            providers: ProviderRegistry::builtins(),
            refresh_guard: Default::default(),
            todo_guard: Default::default(),
            todo_sequence: AtomicU64::new(1),
            _notifications: NotificationSink::unavailable(),
        };
        let todo = service
            .create_todo("Write tests".into(), None, Some("2026-01-20".into()), 3)
            .await
            .unwrap();
        let from = 1_767_225_600_000;
        let to = 1_769_904_000_000;
        assert_eq!(service.query_range(from, to).await.unwrap().todos.len(), 1);
        let completed = service.complete_todo(&todo.id, true).await.unwrap();
        assert!(completed.completed);
        service.delete_todo(&todo.id).await.unwrap();
        assert!(
            service
                .query_range(from, to)
                .await
                .unwrap()
                .todos
                .is_empty()
        );
    }

    #[tokio::test]
    async fn refreshes_multiple_local_calendar_sources() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.ics");
        let second = directory.path().join("second.ics");
        tokio::fs::write(
            &first,
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:first\nSUMMARY:First\nDTSTART:20260115T090000Z\nDTEND:20260115T100000Z\nEND:VEVENT\nEND:VCALENDAR\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            &second,
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:second\nSUMMARY:Second\nDTSTART;VALUE=DATE:20260120\nDTEND;VALUE=DATE:20260121\nEND:VEVENT\nEND:VCALENDAR\n",
        )
        .await
        .unwrap();
        let config_path = directory.path().join("activity.json");
        tokio::fs::write(
            &config_path,
            format!(
                r##"{{"calendar_sources":[{{"id":"one","kind":"ics-file","path":"{}"}},{{"id":"two","kind":"ics-file","path":"{}"}}]}}"##,
                first.display(),
                second.display()
            ),
        )
        .await
        .unwrap();
        let state = StateStore::default();
        let service = ActivityService {
            data: Default::default(),
            state: state.clone(),
            config_path,
            todo_path: directory.path().join("todos.json"),
            providers: ProviderRegistry::builtins(),
            refresh_guard: Default::default(),
            todo_guard: Default::default(),
            todo_sequence: AtomicU64::new(1),
            _notifications: NotificationSink::unavailable(),
        };
        service.refresh().await;
        let snapshot = state.snapshot().await.activity;
        assert_eq!(snapshot.event_count, 2);
        assert_eq!(snapshot.sources.len(), 2);
        assert!(snapshot.sources.iter().all(|source| source.available));
        let range = service
            .query_range(1_767_225_600_000, 1_769_904_000_000)
            .await
            .unwrap();
        assert_eq!(range.events.len(), 2);
        assert_eq!(range.busy_dates, vec!["2026-01-15", "2026-01-20"]);
    }

    #[tokio::test]
    async fn rejects_inverted_ranges() {
        let directory = tempdir().unwrap();
        let service = ActivityService {
            data: Default::default(),
            state: StateStore::default(),
            config_path: directory.path().join("activity.json"),
            todo_path: directory.path().join("todos.json"),
            providers: ProviderRegistry::builtins(),
            refresh_guard: Default::default(),
            todo_guard: Default::default(),
            todo_sequence: AtomicU64::new(1),
            _notifications: NotificationSink::unavailable(),
        };
        assert!(service.query_range(10, 10).await.is_err());
    }
}
