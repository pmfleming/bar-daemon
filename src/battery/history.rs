use std::{
    collections::VecDeque,
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    model::{BatteryHistoryPoint, BatteryHistoryState, BatteryState},
    time::unix_ms as now_milliseconds,
};

const FILE_VERSION: u8 = 1;
const RETENTION_DAYS: u8 = 7;
const RETENTION_MILLISECONDS: u64 = RETENTION_DAYS as u64 * 24 * 60 * 60 * 1_000;
const BUCKET_MILLISECONDS: u64 = 15 * 60 * 1_000;

static HISTORY: OnceLock<Mutex<HistoryStore>> = OnceLock::new();

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct HistoryFile {
    version: u8,
    last_charge_timestamp_ms: u64,
    points: Vec<BatteryHistoryPoint>,
}

#[derive(Debug)]
struct HistoryStore {
    path: Option<PathBuf>,
    last_charge_timestamp_ms: u64,
    points: VecDeque<BatteryHistoryPoint>,
}

impl HistoryStore {
    fn load(path: Option<PathBuf>, now_ms: u64) -> Self {
        let file = path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<HistoryFile>(&bytes).ok())
            .filter(|file| file.version == FILE_VERSION)
            .unwrap_or_default();
        let mut store = Self {
            path,
            last_charge_timestamp_ms: file.last_charge_timestamp_ms,
            points: file.points.into(),
        };
        store.prune(now_ms);
        store
    }

    fn record(&mut self, state: &BatteryState, now_ms: u64) -> bool {
        if !state.available {
            return false;
        }
        self.prune(now_ms);
        let previous = self.points.back();
        let power_transition = previous.is_some_and(|point| {
            point.plugged != state.plugged || point.charging != state.charging
        });
        if !state.plugged
            && (self.last_charge_timestamp_ms == 0 || previous.is_some_and(|point| point.plugged))
        {
            self.last_charge_timestamp_ms = now_ms;
        }
        let current_bucket = now_ms - now_ms % BUCKET_MILLISECONDS;
        let bucket_changed = previous.is_none_or(|point| {
            point.timestamp_ms - point.timestamp_ms % BUCKET_MILLISECONDS != current_bucket
        });
        if !bucket_changed && !power_transition {
            return false;
        }
        self.points.push_back(BatteryHistoryPoint {
            timestamp_ms: now_ms,
            percentage: state.percentage,
            power_watts: finite_nonnegative(state.power_watts),
            time_to_full_seconds: (state.charging && state.time_to_full_seconds > 0)
                .then_some(state.time_to_full_seconds),
            charging: state.charging,
            plugged: state.plugged,
        });
        self.prune(now_ms);
        true
    }

    fn state(&self, include_points: bool) -> BatteryHistoryState {
        BatteryHistoryState {
            retention_days: RETENTION_DAYS,
            last_charge_timestamp_ms: self.last_charge_timestamp_ms,
            latest_timestamp_ms: self.points.back().map_or(0, |point| point.timestamp_ms),
            points: if include_points {
                self.points.iter().cloned().collect()
            } else {
                Vec::new()
            },
        }
    }

    fn prune(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(RETENTION_MILLISECONDS);
        while self
            .points
            .front()
            .is_some_and(|point| point.timestamp_ms < cutoff)
        {
            self.points.pop_front();
        }
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = HistoryFile {
            version: FILE_VERSION,
            last_charge_timestamp_ms: self.last_charge_timestamp_ms,
            points: self.points.iter().cloned().collect(),
        };
        let temporary = temporary_path(path);
        fs::write(&temporary, serde_json::to_vec(&file)?)?;
        fs::rename(temporary, path)
    }
}

pub(super) fn attach_summary(mut state: BatteryState) -> BatteryState {
    let now_ms = now_milliseconds();
    let history = shared_history(now_ms);
    let mut history = history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if history.record(&state, now_ms)
        && let Err(error) = history.persist()
    {
        tracing::warn!(%error, "battery history could not be saved");
    }
    state.history = history.state(false);
    state
}

pub(super) fn snapshot() -> BatteryHistoryState {
    let now_ms = now_milliseconds();
    let history = shared_history(now_ms);
    let mut history = history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    history.prune(now_ms);
    history.state(true)
}

fn shared_history(now_ms: u64) -> &'static Mutex<HistoryStore> {
    HISTORY.get_or_init(|| Mutex::new(HistoryStore::load(history_path(), now_ms)))
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        (value * 100.0).round() / 100.0
    } else {
        0.0
    }
}

fn history_path() -> Option<PathBuf> {
    env::var_os("BAR_DAEMON_BATTERY_HISTORY")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
                })
                .map(|root| root.join("bar-daemon/battery-history-v1.json"))
        })
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".tmp");
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::HistoryStore;
    use crate::model::BatteryState;

    fn state(percentage: u8, plugged: bool, charging: bool) -> BatteryState {
        BatteryState {
            available: true,
            percentage,
            plugged,
            charging,
            time_to_full_seconds: 3_600,
            power_watts: 12.5,
            ..BatteryState::default()
        }
    }

    #[test]
    fn records_buckets_and_power_transitions() {
        let mut history = HistoryStore::load(None, 1_000);
        assert!(history.record(&state(80, true, true), 1_000));
        assert!(!history.record(&state(81, true, true), 2_000));
        assert!(history.record(&state(81, false, false), 3_000));
        assert_eq!(history.last_charge_timestamp_ms, 3_000);
        assert_eq!(history.points.len(), 2);
        assert_eq!(history.points[0].time_to_full_seconds, Some(3_600));
        assert_eq!(history.points[1].time_to_full_seconds, None);
        let summary = history.state(false);
        assert_eq!(summary.latest_timestamp_ms, 3_000);
        assert!(summary.points.is_empty());
        assert_eq!(history.state(true).points.len(), 2);
    }

    #[test]
    fn keeps_seven_days_only() {
        let day = 24 * 60 * 60 * 1_000;
        let mut history = HistoryStore::load(None, 0);
        history.record(&state(90, true, true), 1);
        history.record(&state(80, false, false), 8 * day);
        assert_eq!(history.points.len(), 1);
    }
}
