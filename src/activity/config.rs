use std::{collections::HashSet, env, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ActivityConfig {
    pub calendar_sources: Vec<CalendarSourceConfig>,
    pub world_clocks: Vec<WorldClockConfig>,
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

pub(crate) fn config_path() -> PathBuf {
    env::var_os("BAR_DAEMON_ACTIVITY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_home().join("bar-daemon/activity.json"))
}

pub(crate) fn todo_path() -> PathBuf {
    env::var_os("BAR_DAEMON_TODO_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_home().join("bar-daemon/todos.json"))
}

pub(crate) fn notification_database_path() -> PathBuf {
    env::var_os("BAR_DAEMON_NOTIFICATION_DATABASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_home().join("bar-daemon/notifications.sqlite3"))
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

fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn state_home() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
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
              "world_clocks": [{"timezone":"Asia/Tokyo","label":"Tokyo"}]
            }"##,
        )
        .unwrap();
        assert_eq!(config.calendar_sources.len(), 2);
        assert_eq!(config.calendar_sources[1].color, "#7aa2f7");
        assert_eq!(config.world_clocks[0].timezone, "Asia/Tokyo");
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
