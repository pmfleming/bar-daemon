use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(crate) const CHARGE_ONCE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const CALIBRATION_MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BatteryConfig {
    pub warning_percent: u8,
    pub critical_percent: u8,
    pub notify_when_full: bool,
    pub auto_power_saver: bool,
    pub manage_thresholds: bool,
    pub protection_enabled: bool,
    pub protected_start_percent: u8,
    pub protected_end_percent: u8,
    pub devices: BTreeMap<String, BatteryDeviceConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BatteryDeviceConfig {
    pub manage_thresholds: bool,
    pub protection_enabled: bool,
    pub protected_start_percent: u8,
    pub protected_end_percent: u8,
    pub accepted_reported_start_percent: Option<u8>,
    pub accepted_reported_end_percent: Option<u8>,
}

impl Default for BatteryDeviceConfig {
    fn default() -> Self {
        Self {
            manage_thresholds: false,
            protection_enabled: false,
            protected_start_percent: 75,
            protected_end_percent: 80,
            accepted_reported_start_percent: None,
            accepted_reported_end_percent: None,
        }
    }
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self {
            warning_percent: 25,
            critical_percent: 12,
            notify_when_full: true,
            auto_power_saver: true,
            manage_thresholds: false,
            protection_enabled: false,
            protected_start_percent: 75,
            protected_end_percent: 80,
            devices: BTreeMap::new(),
        }
    }
}

impl BatteryConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.critical_percent > self.warning_percent || self.warning_percent > 100 {
            bail!("alert percentages must satisfy 0 <= critical <= warning <= 100");
        }
        if self.protected_start_percent >= self.protected_end_percent
            || self.protected_end_percent > 100
        {
            bail!("protection thresholds must satisfy 0 <= start < end <= 100");
        }
        for (battery_id, device) in &self.devices {
            validate_battery_id(battery_id)?;
            device.validate()?;
        }
        Ok(())
    }

    pub(crate) fn device(&self, battery_id: &str) -> BatteryDeviceConfig {
        self.devices
            .get(battery_id)
            .cloned()
            .unwrap_or_else(|| BatteryDeviceConfig {
                manage_thresholds: self.manage_thresholds,
                protection_enabled: self.protection_enabled,
                protected_start_percent: self.protected_start_percent,
                protected_end_percent: self.protected_end_percent,
                ..BatteryDeviceConfig::default()
            })
    }

    pub(crate) fn device_mut(&mut self, battery_id: &str) -> &mut BatteryDeviceConfig {
        let inherited = self.device(battery_id);
        self.devices
            .entry(battery_id.to_string())
            .or_insert(inherited)
    }
}

impl BatteryDeviceConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.protected_start_percent >= self.protected_end_percent
            || self.protected_end_percent > 100
        {
            bail!("protection thresholds must satisfy 0 <= start < end <= 100");
        }
        match (
            self.accepted_reported_start_percent,
            self.accepted_reported_end_percent,
        ) {
            (Some(_), Some(_)) | (None, None) => Ok(()),
            _ => bail!("accepted reported thresholds must both be present or absent"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BatteryRuntimeState {
    pub charge_once_active: bool,
    pub charge_once_battery_id: String,
    pub charge_once_started_unix_ms: u64,
    pub charge_once_expires_unix_ms: u64,
    pub restore_start_percent: Option<u8>,
    pub restore_end_percent: Option<u8>,
    pub operation: String,
    pub operation_battery_id: String,
    pub operation_phase: String,
    pub operation_started_unix_ms: u64,
    pub operation_expires_unix_ms: u64,
    pub operation_restore_start_percent: Option<u8>,
    pub operation_restore_end_percent: Option<u8>,
}

impl BatteryRuntimeState {
    pub(crate) fn start_charge_once(
        now_unix_ms: u64,
        battery_id: String,
        restore_start_percent: u8,
        restore_end_percent: u8,
    ) -> Self {
        Self {
            charge_once_active: true,
            charge_once_battery_id: battery_id,
            charge_once_started_unix_ms: now_unix_ms,
            charge_once_expires_unix_ms: now_unix_ms
                .saturating_add(CHARGE_ONCE_MAX_AGE.as_millis() as u64),
            restore_start_percent: Some(restore_start_percent),
            restore_end_percent: Some(restore_end_percent),
            ..Self::default()
        }
    }

    pub(crate) fn is_expired(&self, now_unix_ms: u64) -> bool {
        self.charge_once_active
            && self.charge_once_expires_unix_ms != 0
            && now_unix_ms >= self.charge_once_expires_unix_ms
    }

    pub(crate) fn start_inhibit(now_unix_ms: u64, battery_id: String) -> Self {
        Self {
            operation: "inhibit".into(),
            operation_battery_id: battery_id,
            operation_phase: "paused".into(),
            operation_started_unix_ms: now_unix_ms,
            ..Self::default()
        }
    }

    pub(crate) fn start_calibration(
        now_unix_ms: u64,
        battery_id: String,
        restore_start_percent: u8,
        restore_end_percent: u8,
    ) -> Self {
        Self {
            operation: "calibration".into(),
            operation_battery_id: battery_id,
            operation_phase: "discharging".into(),
            operation_started_unix_ms: now_unix_ms,
            operation_expires_unix_ms: now_unix_ms
                .saturating_add(CALIBRATION_MAX_AGE.as_millis() as u64),
            operation_restore_start_percent: Some(restore_start_percent),
            operation_restore_end_percent: Some(restore_end_percent),
            ..Self::default()
        }
    }

    pub(crate) fn operation_expired(&self, now_unix_ms: u64) -> bool {
        !self.operation.is_empty()
            && self.operation_expires_unix_ms != 0
            && now_unix_ms >= self.operation_expires_unix_ms
    }

    pub(crate) fn clear_operation(&mut self) {
        self.operation.clear();
        self.operation_battery_id.clear();
        self.operation_phase.clear();
        self.operation_started_unix_ms = 0;
        self.operation_expires_unix_ms = 0;
        self.operation_restore_start_percent = None;
        self.operation_restore_end_percent = None;
    }
}

fn validate_battery_id(battery_id: &str) -> Result<()> {
    let suffix = battery_id
        .strip_prefix("BAT")
        .with_context(|| format!("battery id {battery_id} must start with BAT"))?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("battery id must match BAT followed by digits");
    }
    Ok(())
}

pub(crate) fn config_path() -> PathBuf {
    env::var_os("BAR_DAEMON_BATTERY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_home().join("bar-daemon/battery.json"))
}

pub(crate) fn state_path() -> PathBuf {
    env::var_os("BAR_DAEMON_BATTERY_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_home().join("bar-daemon/battery-state.json"))
}

pub(crate) async fn load_config() -> Result<BatteryConfig> {
    let config: BatteryConfig = load_or_default(&config_path()).await?;
    config.validate()?;
    Ok(config)
}

pub(crate) async fn save_config(config: &BatteryConfig) -> Result<()> {
    config.validate()?;
    save_atomic(&config_path(), config).await
}

pub(crate) async fn load_runtime() -> Result<BatteryRuntimeState> {
    load_or_default(&state_path()).await
}

pub(crate) async fn save_runtime(state: &BatteryRuntimeState) -> Result<()> {
    save_atomic(&state_path(), state).await
}

async fn load_or_default<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned + Default,
{
    match tokio::fs::read(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .with_context(|| format!("parse battery data {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

async fn save_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("battery data path {} has no parent", path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("create {}", parent.display()))?;
    let temporary = path.with_extension("json.tmp");
    let contents = serde_json::to_vec_pretty(value)?;
    tokio::fs::write(&temporary, contents)
        .await
        .with_context(|| format!("write {}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace {}", path.display()))
}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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
    use super::{BatteryConfig, BatteryRuntimeState, CALIBRATION_MAX_AGE, CHARGE_ONCE_MAX_AGE};

    #[test]
    fn config_defaults_are_conservative() {
        let config: BatteryConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.manage_thresholds);
        assert!(!config.protection_enabled);
        assert_eq!(config.protected_start_percent, 75);
        assert_eq!(config.protected_end_percent, 80);
        assert!(config.auto_power_saver);
        config.validate().unwrap();
    }

    #[test]
    fn validates_alert_and_protection_ranges() {
        assert!(
            BatteryConfig {
                warning_percent: 10,
                critical_percent: 20,
                ..BatteryConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            BatteryConfig {
                protected_start_percent: 80,
                protected_end_percent: 80,
                ..BatteryConfig::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn charge_once_has_a_bounded_lifetime() {
        let runtime = BatteryRuntimeState::start_charge_once(1_000, "BAT0".into(), 75, 80);
        let expires = 1_000 + CHARGE_ONCE_MAX_AGE.as_millis() as u64;
        assert_eq!(runtime.charge_once_expires_unix_ms, expires);
        assert!(!runtime.is_expired(expires - 1));
        assert!(runtime.is_expired(expires));
        assert_eq!(runtime.restore_start_percent, Some(75));
        assert_eq!(runtime.restore_end_percent, Some(80));
        assert_eq!(runtime.charge_once_battery_id, "BAT0");
    }

    #[test]
    fn calibration_has_a_bounded_lifetime_and_restore_range() {
        let state = BatteryRuntimeState::start_calibration(1_000, "BAT1".into(), 70, 85);
        assert_eq!(state.operation, "calibration");
        assert_eq!(state.operation_battery_id, "BAT1");
        assert_eq!(state.operation_restore_start_percent, Some(70));
        assert_eq!(state.operation_restore_end_percent, Some(85));
        assert_eq!(
            state.operation_expires_unix_ms,
            1_000 + CALIBRATION_MAX_AGE.as_millis() as u64
        );
    }

    #[tokio::test]
    async fn persisted_data_round_trips() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("nested/battery.json");
        let expected = BatteryConfig {
            manage_thresholds: true,
            protection_enabled: true,
            ..BatteryConfig::default()
        };
        super::save_atomic(&path, &expected).await.unwrap();
        let actual: BatteryConfig = super::load_or_default(&path).await.unwrap();
        assert_eq!(actual, expected);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn device_policy_inherits_legacy_defaults_then_diverges() {
        let mut config = BatteryConfig {
            manage_thresholds: true,
            protection_enabled: true,
            protected_start_percent: 70,
            protected_end_percent: 85,
            ..BatteryConfig::default()
        };
        assert_eq!(config.device("BAT0").protected_start_percent, 70);
        config.device_mut("BAT1").protected_end_percent = 90;
        assert_eq!(config.device("BAT1").protected_end_percent, 90);
        assert_eq!(config.device("BAT0").protected_end_percent, 85);
        config.validate().unwrap();
    }
}
