use anyhow::{Context, Result};
use chrono::{Offset, Utc};
use chrono_tz::Tz;
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
    pub lunar: Option<super::astronomy::LunarPhase>,
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
    pub timezone_region_ids: Vec<String>,
}

impl WorldClockState {
    pub(super) fn new(timezone: &str, label: &str) -> Result<Self> {
        let zone: Tz = timezone
            .parse()
            .with_context(|| format!("parse world-clock timezone {timezone}"))?;
        let instant = Utc::now();
        let now = instant.with_timezone(&zone);
        let city = timezone
            .rsplit('/')
            .next()
            .unwrap_or(timezone)
            .replace('_', " ");
        let utc_offset_seconds = now.offset().fix().local_minus_utc();
        Ok(Self {
            timezone: timezone.into(),
            label: if label.is_empty() {
                city.clone()
            } else {
                label.into()
            },
            city,
            abbreviation: now.format("%Z").to_string(),
            utc_offset_seconds,
            timezone_region_ids: crate::timezone_regions::ids_for_offset(
                utc_offset_seconds,
                instant,
            ),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WeatherState {
    pub available: bool,
    pub id: String,
    pub location: String,
    pub home: bool,
    pub timezone: String,
    pub utc_offset_seconds: i32,
    pub timezone_region_ids: Vec<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub condition: String,
    pub condition_code: u16,
    pub is_day: bool,
    pub temperature_c: f64,
    pub apparent_temperature_c: f64,
    pub high_c: f64,
    pub low_c: f64,
    pub precipitation_probability: u8,
    pub precipitation_mm: f64,
    pub wind_speed_kmh: f64,
    pub wind_direction_degrees: u16,
    pub wind_gust_kmh: f64,
    pub humidity_percent: u8,
    pub sunrise_unix_ms: i64,
    pub sunset_unix_ms: i64,
    pub updated_unix_ms: i64,
    pub solar_noon: Option<super::astronomy::SolarNoon>,
    pub hourly: Vec<WeatherHour>,
    pub daily: Vec<WeatherDay>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WeatherHour {
    pub time_unix_ms: i64,
    pub temperature_c: f64,
    pub precipitation_probability: u8,
    pub precipitation_mm: f64,
    pub wind_speed_kmh: f64,
    pub wind_direction_degrees: u16,
    pub condition: String,
    pub condition_code: u16,
    pub is_day: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WeatherDay {
    pub date_unix_ms: i64,
    pub high_c: f64,
    pub low_c: f64,
    pub precipitation_probability: u8,
    pub condition: String,
    pub condition_code: u16,
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
    pub days: std::collections::BTreeMap<String, ActivityDay>,
    pub local_date: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActivityDay {
    pub event_ids: Vec<String>,
    pub todo_ids: Vec<String>,
}
