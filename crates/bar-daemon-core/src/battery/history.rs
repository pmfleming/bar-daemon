use std::{
    collections::VecDeque,
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    model::{BatteryHistoryPoint, BatteryHistoryState, BatteryState},
    time::unix_ms as now_milliseconds,
};

const FILE_VERSION: u8 = 2;
const RETENTION_DAYS: u8 = 7;
const RETENTION_MILLISECONDS: u64 = RETENTION_DAYS as u64 * 24 * 60 * 60 * 1_000;
const BUCKET_MILLISECONDS: u64 = 15 * 60 * 1_000;
// The native monitor observes state every 30 seconds. A larger gap means the
// daemon could not observe the laptop (normally suspend, shutdown, or restart)
// and must not consume space on the graph's active-only timescale.
const MAX_OBSERVATION_GAP_MILLISECONDS: u64 = 2 * 60 * 1_000;
const LEGACY_POINT_GAP_MILLISECONDS: u64 = BUCKET_MILLISECONDS + MAX_OBSERVATION_GAP_MILLISECONDS;
// Allow minor wall-clock adjustments, but never interpolate across a sleep or
// clock jump. Linux Instant uses CLOCK_MONOTONIC, which excludes suspend time.
const CLOCK_TOLERANCE_MILLISECONDS: u64 = 1_000;

static HISTORY: OnceLock<Mutex<HistoryStore>> = OnceLock::new();

// Deserialize owned points; persist a borrowed deque without copying its samples.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct HistoryFile<P = Vec<BatteryHistoryPoint>> {
    version: u8,
    last_charge_timestamp_ms: u64,
    points: P,
}

#[derive(Debug)]
struct HistoryStore {
    path: Option<PathBuf>,
    last_charge_timestamp_ms: u64,
    points: VecDeque<BatteryHistoryPoint>,
    active_time_ms: u64,
    last_observation: Option<(u64, Instant)>,
    current_point: Option<BatteryHistoryPoint>,
    energy: OnceLock<crate::model::EnergyHistory>,
}

impl HistoryStore {
    fn load(path: Option<PathBuf>, now_ms: u64) -> Self {
        let mut file = path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<HistoryFile>(&bytes).ok())
            .filter(|file| (1..=FILE_VERSION).contains(&file.version))
            .unwrap_or_default();
        if file.version < FILE_VERSION {
            migrate_legacy_points(&mut file.points);
        }
        normalize_modes(&mut file.points);
        let active_time_ms = file.points.last().map_or(0, |point| point.active_time_ms);
        let mut store = Self {
            path,
            last_charge_timestamp_ms: file.last_charge_timestamp_ms,
            points: file.points.into(),
            active_time_ms,
            // A process start always begins a new graph segment. This also
            // prevents downtime before startup from entering the timescale.
            last_observation: None,
            current_point: None,
            energy: OnceLock::new(),
        };
        store.prune(now_ms);
        store
    }

    fn record(&mut self, state: &BatteryState, now_ms: u64, awake_now: Instant) -> bool {
        if !state.available {
            self.last_observation = None;
            self.current_point = None;
            return false;
        }
        self.prune(now_ms);

        let active_delta_ms = self.last_observation.and_then(|(wall, awake)| {
            let wall_delta = now_ms.checked_sub(wall)?;
            let awake_delta: u64 = awake_now
                .checked_duration_since(awake)?
                .as_millis()
                .try_into()
                .ok()?;
            (awake_delta <= MAX_OBSERVATION_GAP_MILLISECONDS
                && wall_delta.abs_diff(awake_delta) <= CLOCK_TOLERANCE_MILLISECONDS)
                .then_some(awake_delta)
        });
        let observation_continuous = active_delta_ms.is_some();
        self.active_time_ms = self
            .active_time_ms
            .saturating_add(active_delta_ms.unwrap_or_default());

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
        self.last_observation = Some((now_ms, awake_now));
        let point = BatteryHistoryPoint {
            timestamp_ms: now_ms,
            active_time_ms: self.active_time_ms,
            continuous: observation_continuous && previous.is_some(),
            mode: mode(state.charging, state.plugged).into(),
            percentage: state.percentage,
            power_watts: finite_nonnegative(state.power_watts),
            power_valid: Some(
                state.power_available && state.power_watts.is_finite() && state.power_watts >= 0.0,
            ),
            time_to_full_seconds: (state.charging && state.time_to_full_seconds > 0)
                .then_some(state.time_to_full_seconds),
            charging: state.charging,
            plugged: state.plugged,
        };
        let sampled = !observation_continuous || bucket_changed || power_transition;
        if sampled {
            self.energy.take();
            self.points.push_back(point.clone());
            self.prune(now_ms);
        }
        self.current_point = Some(point);
        sampled
    }

    fn state(&self, include_points: bool) -> BatteryHistoryState {
        let first_active_time_ms = self
            .points
            .front()
            .map_or(self.active_time_ms, |point| point.active_time_ms);
        let active_duration_ms = self.points.back().map_or(0, |point| {
            point.active_time_ms.saturating_sub(first_active_time_ms)
        });
        let points: Vec<_> = if include_points {
            self.points
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, mut point)| {
                    point.active_time_ms =
                        point.active_time_ms.saturating_sub(first_active_time_ms);
                    if index == 0 {
                        point.continuous = false;
                    }
                    point
                })
                .collect()
        } else {
            Vec::new()
        };
        BatteryHistoryState {
            retention_days: RETENTION_DAYS,
            last_charge_timestamp_ms: self.last_charge_timestamp_ms,
            latest_timestamp_ms: self.points.back().map_or(0, |point| point.timestamp_ms),
            active_duration_ms,
            energy: if include_points {
                self.energy
                    .get_or_init(|| super::derived::energy(&points))
                    .clone()
            } else {
                Default::default()
            },
            points,
            current_point: self.current_point.clone().map(|mut point| {
                point.active_time_ms = point.active_time_ms.saturating_sub(first_active_time_ms);
                point
            }),
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
            self.energy.take();
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
            points: &self.points,
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
    if history.record(&state, now_ms, Instant::now())
        && let Err(error) = history.persist()
    {
        tracing::warn!(%error, "battery history could not be saved");
    }
    state.history = history.state(false);
    state.forecast = super::derived::forecast(&state);
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

fn migrate_legacy_points(points: &mut [BatteryHistoryPoint]) {
    let mut active_time_ms = 0_u64;
    let mut previous_timestamp_ms = None;
    for point in points {
        let active_delta_ms = previous_timestamp_ms
            .and_then(|timestamp| point.timestamp_ms.checked_sub(timestamp))
            .filter(|delta| *delta <= LEGACY_POINT_GAP_MILLISECONDS);
        point.continuous = active_delta_ms.is_some();
        active_time_ms = active_time_ms.saturating_add(active_delta_ms.unwrap_or_default());
        point.active_time_ms = active_time_ms;
        previous_timestamp_ms = Some(point.timestamp_ms);
    }
}

fn normalize_modes(points: &mut [BatteryHistoryPoint]) {
    for point in points {
        if point.mode.is_empty() {
            point.mode = mode(point.charging, point.plugged).into();
        }
    }
}

const fn mode(charging: bool, plugged: bool) -> &'static str {
    if charging {
        "charging"
    } else if plugged {
        "holding"
    } else {
        "discharging"
    }
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
