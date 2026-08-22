use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActivityState {
    pub available: bool,
    pub syncing: bool,
    pub event_count: u32,
    pub incomplete_todo_count: u32,
    pub next_event: Option<ActivityEvent>,
    pub sources: Vec<ActivitySourceState>,
    pub world_clocks: Vec<WorldClockState>,
    pub weather: WeatherState,
    pub weather_locations: Vec<WeatherState>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActivitySourceState {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub available: bool,
    pub item_count: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivityEvent {
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
pub(crate) struct TodoItem {
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
pub(crate) struct WorldClockState {
    pub timezone: String,
    pub label: String,
    pub city: String,
    pub abbreviation: String,
    pub utc_offset_seconds: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WeatherState {
    pub available: bool,
    pub id: String,
    pub location: String,
    pub home: bool,
    pub timezone: String,
    pub utc_offset_seconds: i32,
    pub condition: String,
    pub temperature_c: f64,
    pub apparent_temperature_c: f64,
    pub high_c: f64,
    pub low_c: f64,
    pub precipitation_probability: u8,
    pub wind_speed_kmh: f64,
    pub humidity_percent: u8,
    pub sunrise_unix_ms: i64,
    pub sunset_unix_ms: i64,
    pub updated_unix_ms: i64,
    pub hourly: Vec<WeatherHour>,
    pub daily: Vec<WeatherDay>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WeatherHour {
    pub time_unix_ms: i64,
    pub temperature_c: f64,
    pub precipitation_probability: u8,
    pub wind_speed_kmh: f64,
    pub condition: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WeatherDay {
    pub date_unix_ms: i64,
    pub high_c: f64,
    pub low_c: f64,
    pub precipitation_probability: u8,
    pub condition: String,
    pub sunrise_unix_ms: i64,
    pub sunset_unix_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActivityRange {
    pub from_unix_ms: i64,
    pub to_unix_ms: i64,
    pub events: Vec<ActivityEvent>,
    pub todos: Vec<TodoItem>,
    pub busy_dates: Vec<String>,
}
