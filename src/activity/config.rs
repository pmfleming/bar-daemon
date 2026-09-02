use std::{collections::HashSet, env, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use shelllist_daemon_core::XdgRoot;

use crate::paths::data_file;

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
    let mut ids = HashSet::new();
    for source in &config.calendar_sources {
        if source.id.trim().is_empty() {
            anyhow::bail!("calendar source id cannot be empty");
        }
        if !ids.insert(source.id.as_str()) {
            anyhow::bail!("duplicate calendar source id {}", source.id);
        }
        if source.path.as_os_str().is_empty() {
            anyhow::bail!("calendar source {} path cannot be empty", source.id);
        }
        if source.kind.trim().is_empty() {
            anyhow::bail!("calendar source {} kind cannot be empty", source.id);
        }
    }
    for clock in &config.world_clocks {
        clock
            .timezone
            .parse::<chrono_tz::Tz>()
            .with_context(|| format!("parse world-clock timezone {}", clock.timezone))?;
    }
    let weather_locations = config.configured_weather_locations();
    let mut weather_ids = HashSet::new();
    let mut home_count = 0;
    for weather in &weather_locations {
        if !(-90.0..=90.0).contains(&weather.latitude) {
            anyhow::bail!("weather latitude must be between -90 and 90");
        }
        if !(-180.0..=180.0).contains(&weather.longitude) {
            anyhow::bail!("weather longitude must be between -180 and 180");
        }
        if weather.id.trim().is_empty() {
            anyhow::bail!("weather location id cannot be empty");
        }
        if !weather_ids.insert(weather.id.as_str()) {
            anyhow::bail!("duplicate weather location id {}", weather.id);
        }
        if weather.location.trim().is_empty() {
            anyhow::bail!("weather location cannot be empty");
        }
        if weather.timezone.trim().is_empty() {
            anyhow::bail!("weather timezone cannot be empty");
        }
        home_count += usize::from(weather.home);
    }
    if home_count > 1 {
        anyhow::bail!("only one weather location can be marked as home");
    }
    Ok(())
}

pub(crate) async fn load(path: &PathBuf) -> Result<ActivityConfig> {
    match tokio::fs::read(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .with_context(|| format!("parse activity configuration {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ActivityConfig::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::ActivityConfig;

    #[test]
    fn parses_multiple_sources_and_world_clocks() {
        let config: ActivityConfig = serde_json::from_str(
            r##"{
              "calendar_sources": [
                {"id":"work","name":"Work","kind":"ics-directory","path":"/tmp/work","color":"#123456"},
                {"id":"home","name":"Home","kind":"ics-file","path":"/tmp/home.ics"}
              ],
              "world_clocks": [{"timezone":"Asia/Tokyo","label":"Tokyo"}],
              "weather_locations": [
                {"id":"home","location":"Amsterdam","home":true,"latitude":52.37,"longitude":4.90,"timezone":"Europe/Amsterdam"},
                {"id":"dublin","location":"Dublin","latitude":53.35,"longitude":-6.26,"timezone":"Europe/Dublin"}
              ]
            }"##,
        )
        .unwrap();
        assert_eq!(config.calendar_sources.len(), 2);
        assert_eq!(config.calendar_sources[1].color, "#7aa2f7");
        assert_eq!(config.world_clocks[0].timezone, "Asia/Tokyo");
        assert_eq!(config.weather_locations[0].location, "Amsterdam");
        super::validate(&config).unwrap();
    }

    #[test]
    fn documented_example_is_valid() {
        let config: ActivityConfig =
            serde_json::from_str(include_str!("../../docs/activity.example.json")).unwrap();
        super::validate(&config).unwrap();
        assert!(!config.calendar_sources.is_empty());
        assert!(!config.world_clocks.is_empty());
    }

    #[test]
    fn rejects_duplicate_source_ids() {
        let config: ActivityConfig = serde_json::from_str(
            r#"{"calendar_sources":[{"id":"same","path":"/a"},{"id":"same","path":"/b"}]}"#,
        )
        .unwrap();
        assert!(super::validate(&config).is_err());
    }
}
