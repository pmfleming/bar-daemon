use serde::{Deserialize, Serialize};

use crate::activity::notifications::model::ActiveNotification;

pub(crate) use crate::activity::model::ActivityState;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BarSnapshot {
    pub activity: ActivityState,
    pub workspaces: WorkspaceState,
    pub media: MediaState,
    pub audio: AudioState,
    pub brightness: BrightnessState,
    pub battery: BatteryState,
    pub power_profile: PowerProfileState,
    pub power_sleep: PowerSleepState,
    pub notifications: NotificationState,
    pub notification_active: NotificationActiveState,
    pub updates: UpdateState,
    pub timezone: TimezoneState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PowerSleepState {
    pub available: bool,
    pub can_suspend: String,
    pub can_hibernate: String,
    pub preparing_for_sleep: bool,
    pub lock_before_sleep: bool,
    pub inhibitors: Vec<SleepInhibitor>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SleepInhibitor {
    pub what: String,
    pub who: String,
    pub why: String,
    pub mode: String,
    pub uid: u32,
    pub pid: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct TimezoneState {
    pub available: bool,
    pub timezone: String,
    pub city: String,
    pub abbreviation: String,
    pub utc_offset_seconds: i32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UpdateState {
    pub available: bool,
    pub ready: bool,
    pub lanes: Vec<UpdateLane>,
    pub state_directory: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UpdateLane {
    pub name: String,
    pub ready: bool,
    pub revision: Option<String>,
    pub base_hash: Option<String>,
    pub created_at: Option<u64>,
    pub auto_apply: bool,
    pub system: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NotificationActiveState {
    pub available: bool,
    pub revision: u64,
    pub notifications: Vec<ActiveNotification>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NotificationState {
    pub available: bool,
    pub count: u32,
    pub dnd: bool,
    pub inhibited: bool,
    pub text: String,
    pub tooltip: String,
    pub alt: String,
    pub class_name: String,
    pub backend: String,
    pub history_revision: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PowerProfileState {
    pub available: bool,
    pub profile: String,
    pub driver: String,
    pub profiles: Vec<PowerProfile>,
    pub performance_degraded: String,
    pub version: String,
    pub battery_aware: Option<bool>,
    pub actions: Vec<PowerProfileAction>,
    pub active_holds: Vec<PowerProfileHold>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PowerProfile {
    pub name: String,
    pub driver: String,
    pub platform_driver: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PowerProfileAction {
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PowerProfileHold {
    pub application_id: String,
    pub profile: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryState {
    pub available: bool,
    pub native_path: String,
    pub percentage: u8,
    pub state: String,
    pub charging: bool,
    pub plugged: bool,
    pub power_watts: f64,
    pub time_to_empty_seconds: u64,
    pub time_to_full_seconds: u64,
    pub health_percent: Option<u8>,
    pub cycles: Option<u32>,
    pub warning: bool,
    pub critical: bool,
    pub policy: BatteryPolicyState,
    pub operation: BatteryOperationState,
    pub protection: BatteryProtectionState,
    pub devices: Vec<BatteryDeviceState>,
    pub history: BatteryHistoryState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryHistoryState {
    pub retention_days: u8,
    pub last_charge_timestamp_ms: u64,
    pub latest_timestamp_ms: u64,
    pub points: Vec<BatteryHistoryPoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryHistoryPoint {
    pub timestamp_ms: u64,
    pub percentage: u8,
    pub power_watts: f64,
    pub time_to_full_seconds: Option<u64>,
    pub charging: bool,
    pub plugged: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryOperationState {
    pub kind: String,
    pub battery_id: String,
    pub phase: String,
    pub started_unix_ms: u64,
    pub expires_unix_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryPolicyState {
    pub warning_percent: u8,
    pub critical_percent: u8,
    pub notify_when_full: bool,
    pub auto_power_saver: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryDeviceState {
    pub id: String,
    pub vendor: String,
    pub model: String,
    pub serial: String,
    pub present: bool,
    pub percentage: u8,
    pub state: String,
    pub power_watts: f64,
    pub energy_now_wh: Option<f64>,
    pub energy_full_wh: Option<f64>,
    pub energy_full_design_wh: Option<f64>,
    pub voltage_volts: Option<f64>,
    pub health_percent: Option<u8>,
    pub cycles: Option<u32>,
    pub protection: BatteryProtectionState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BatteryProtectionState {
    pub supported: bool,
    pub backend: String,
    pub managed: bool,
    pub enabled: bool,
    pub start_percent: Option<u8>,
    pub end_percent: Option<u8>,
    pub desired_start_percent: Option<u8>,
    pub desired_end_percent: Option<u8>,
    pub desired_enabled: bool,
    pub thresholds_verified: bool,
    pub charge_once_active: bool,
    pub supports_start: bool,
    pub supports_end: bool,
    pub supports_charge_behaviour: bool,
    pub charge_behaviour: Option<String>,
    pub available_behaviours: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BrightnessState {
    pub available: bool,
    pub device: String,
    pub brightness: u64,
    pub max_brightness: u64,
    pub percent: u8,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AudioState {
    pub available: bool,
    pub sink_name: String,
    pub sink_description: String,
    pub volume_percent: u8,
    pub muted: bool,
    pub input_available: bool,
    pub source_name: String,
    pub source_description: String,
    pub input_muted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct MediaState {
    pub available: bool,
    pub active_player: Option<String>,
    pub players: Vec<MediaPlayer>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct MediaPlayer {
    pub id: String,
    pub identity: String,
    pub desktop_entry: String,
    pub playback_status: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub length_us: u64,
    pub position_us: u64,
    pub position_observed_at_unix_ms: u64,
    pub playback_rate: f64,
    pub can_control: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceState {
    pub available: bool,
    pub focused_monitor: Option<String>,
    pub active_window: Option<ActiveWindow>,
    pub monitors: Vec<MonitorState>,
    pub workspaces: Vec<Workspace>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActiveWindow {
    pub address: String,
    pub title: String,
    pub class_name: String,
    pub initial_class: String,
    pub workspace_id: i64,
    pub fullscreen: bool,
    pub floating: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MonitorState {
    pub id: i64,
    pub name: String,
    pub focused: bool,
    pub active_workspace_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: u32,
    pub urgent: bool,
    pub fullscreen: bool,
    pub last_window_title: String,
}
