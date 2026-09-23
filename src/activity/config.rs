use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use shelllist_daemon_core::XdgRoot;

use crate::paths::{data_file, load_json_or_default, save_json_atomic};

use super::model::TodoItem;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ActivityConfig {
    pub calendar_sources: Vec<CalendarSourceConfig>,
    pub world_clocks: Vec<WorldClockConfig>,
    pub weather_locations: Vec<WeatherConfig>,
    pub weather: Option<WeatherConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct CalendarSourceConfig {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub path: PathBuf,
    pub color: String,
}

impl Default for CalendarSourceConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: "ics-directory".into(),
            path: PathBuf::new(),
            color: "#7aa2f7".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WorldClockConfig {
    pub timezone: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub(crate) struct WeatherConfig {
    pub id: String,
    pub location: String,
    pub home: bool,
    pub latitude: f64,
    pub longitude: f64,
    pub timezone: String,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            location: "Local".into(),
            home: false,
            latitude: 0.0,
            longitude: 0.0,
            timezone: "auto".into(),
        }
    }
}

pub(crate) fn config_path() -> PathBuf {
    env::var_os("BAR_DAEMON_ACTIVITY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_file(XdgRoot::Config, "activity.json"))
}

pub(crate) fn todo_path() -> PathBuf {
    env::var_os("BAR_DAEMON_TODO_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_file(XdgRoot::State, "todos.json"))
}

pub(crate) fn notification_database_path() -> PathBuf {
    env::var_os("BAR_DAEMON_NOTIFICATION_DATABASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_file(XdgRoot::State, "notifications.sqlite3"))
}

impl ActivityConfig {
    pub(crate) fn configured_weather_locations(&self) -> Vec<WeatherConfig> {
        if self.weather_locations.is_empty() {
            self.weather
                .iter()
                .cloned()
                .map(|mut weather| {
                    if weather.id.trim().is_empty() {
                        weather.id = "home".into();
                    }
                    weather.home = true;
                    weather
                })
                .collect()
        } else {
            self.weather_locations.clone()
        }
    }
}

pub(crate) fn validate(config: &ActivityConfig) -> Result<()> {
    validate_calendar_sources(&config.calendar_sources)?;
    validate_world_clocks(&config.world_clocks)?;
    validate_weather(&config.configured_weather_locations())
}

fn validate_calendar_sources(sources: &[CalendarSourceConfig]) -> Result<()> {
    let mut ids = HashSet::new();
    for source in sources {
        non_empty(&source.id, "calendar source id")?;
        anyhow::ensure!(
            ids.insert(source.id.as_str()),
            "duplicate calendar source id {}",
            source.id
        );
        anyhow::ensure!(
            !source.path.as_os_str().is_empty(),
            "calendar source {} path cannot be empty",
            source.id
        );
        non_empty(&source.kind, &format!("calendar source {} kind", source.id))?;
    }
    Ok(())
}

fn validate_world_clocks(clocks: &[WorldClockConfig]) -> Result<()> {
    for clock in clocks {
        clock
            .timezone
            .parse::<chrono_tz::Tz>()
            .with_context(|| format!("parse world-clock timezone {}", clock.timezone))?;
    }
    Ok(())
}

fn validate_weather(locations: &[WeatherConfig]) -> Result<()> {
    let mut ids = HashSet::new();
    let mut home_count = 0;
    for weather in locations {
        coordinate(weather.latitude, -90.0..=90.0, "latitude")?;
        coordinate(weather.longitude, -180.0..=180.0, "longitude")?;
        non_empty(&weather.id, "weather location id")?;
        anyhow::ensure!(
            ids.insert(weather.id.as_str()),
            "duplicate weather location id {}",
            weather.id
        );
        non_empty(&weather.location, "weather location")?;
        non_empty(&weather.timezone, "weather timezone")?;
        home_count += usize::from(weather.home);
    }
    anyhow::ensure!(
        home_count <= 1,
        "only one weather location can be marked as home"
    );
    Ok(())
}

fn non_empty(value: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{label} cannot be empty");
    Ok(())
}

fn coordinate(value: f64, range: std::ops::RangeInclusive<f64>, label: &str) -> Result<()> {
    anyhow::ensure!(
        range.contains(&value),
        "weather {label} must be between {} and {}",
        range.start(),
        range.end()
    );
    Ok(())
}

pub(crate) async fn load(path: &Path) -> Result<ActivityConfig> {
    let config = load_json_or_default(path, "activity configuration").await?;
    validate(&config)?;
    Ok(config)
}

pub(crate) async fn load_todos(path: &Path) -> Result<Vec<TodoItem>> {
    load_json_or_default(path, "todo store").await
}

pub(crate) async fn save_todos(path: &Path, todos: &[TodoItem]) -> Result<()> {
    save_json_atomic(path, todos).await
}

#[cfg(test)]
mod tests {
    use super::ActivityConfig;

    #[test]
    fn documented_config_is_valid_but_duplicate_source_ids_are_rejected() {
        let mut config: ActivityConfig =
            serde_json::from_str(include_str!("../../docs/activity.example.json")).unwrap();
        super::validate(&config).unwrap();
        assert!(!config.calendar_sources.is_empty());
        assert!(!config.world_clocks.is_empty());

        config
            .calendar_sources
            .push(config.calendar_sources[0].clone());
        assert!(super::validate(&config).is_err());
    }
}
