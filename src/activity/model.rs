use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityState {
    pub available: bool,
    pub syncing: bool,
    pub event_count: u32,
    pub incomplete_todo_count: u32,
    pub next_event: Option<ActivityEvent>,
    pub sources: Vec<ActivitySourceState>,
    pub world_clocks: Vec<WorldClockState>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivitySourceState {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub available: bool,
    pub item_count: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: String,
    pub source_id: String,
    pub calendar_name: String,
    pub color: String,
    pub title: String,
    pub start_unix_ms: i64,
    pub end_unix_ms: i64,
    pub all_day: bool,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub timezone: Option<String>,
    pub location: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub source_id: String,
    pub title: String,
    pub completed: bool,
    pub priority: u8,
    pub due_unix_ms: Option<i64>,
    pub due_date: Option<String>,
    pub created_unix_ms: i64,
    pub completed_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldClockState {
    pub timezone: String,
    pub label: String,
    pub city: String,
    pub abbreviation: String,
    pub utc_offset_seconds: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityRange {
    pub from_unix_ms: i64,
    pub to_unix_ms: i64,
    pub events: Vec<ActivityEvent>,
    pub todos: Vec<TodoItem>,
    pub busy_dates: Vec<String>,
}
