use std::{collections::BTreeMap, env, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::XdgRoot;

use crate::{
    model::BatteryProfileAction,
    paths::{data_file, load_json_or_default, save_json_atomic},
};

pub(crate) const CHARGE_ONCE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const CALIBRATION_MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BatteryConfig {
    pub warning_percent: u8,
    pub critical_percent: u8,
    pub notify_when_full: bool,
    pub auto_power_saver: bool,
    pub notify_warning: bool,
    pub notify_critical: bool,
    // Absent in legacy files: inherit auto_power_saver without losing preferences.
    pub warning_profile: Option<BatteryProfileAction>,
    pub critical_profile: Option<BatteryProfileAction>,
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
            notify_warning: true,
            notify_critical: true,
            warning_profile: None,
            critical_profile: None,
            manage_thresholds: false,
            protection_enabled: false,
            protected_start_percent: 75,
            protected_end_percent: 80,
            devices: BTreeMap::new(),
        }
    }
}

impl BatteryConfig {
    pub(crate) fn warning_profile(&self) -> BatteryProfileAction {
        self.warning_profile
            .unwrap_or_else(|| self.legacy_profile())
    }

    pub(crate) fn critical_profile(&self) -> BatteryProfileAction {
        self.critical_profile
            .unwrap_or_else(|| self.legacy_profile())
    }

    fn legacy_profile(&self) -> BatteryProfileAction {
        if self.auto_power_saver {
            BatteryProfileAction::PowerSaver
        } else {
            BatteryProfileAction::KeepCurrent
        }
    }

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OperationKind {
    #[default]
    #[serde(rename = "")]
    None,
    #[serde(rename = "inhibit")]
    Inhibit,
    #[serde(rename = "calibration")]
    Calibration,
}

impl OperationKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Inhibit => "inhibit",
            Self::Calibration => "calibration",
        }
    }
}

impl std::fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OperationPhase {
    #[default]
    #[serde(rename = "")]
    None,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "discharging")]
    Discharging,
    #[serde(rename = "charging")]
    Charging,
    #[serde(rename = "restoring")]
    Restoring,
}

impl OperationPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Paused => "paused",
            Self::Discharging => "discharging",
            Self::Charging => "charging",
            Self::Restoring => "restoring",
        }
    }
}

impl std::fmt::Display for OperationPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
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
    pub operation: OperationKind,
    pub operation_battery_id: String,
    pub operation_phase: OperationPhase,
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
            operation: OperationKind::Inhibit,
            operation_battery_id: battery_id,
            operation_phase: OperationPhase::Paused,
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
            operation: OperationKind::Calibration,
            operation_battery_id: battery_id,
            operation_phase: OperationPhase::Discharging,
            operation_started_unix_ms: now_unix_ms,
            operation_expires_unix_ms: now_unix_ms
                .saturating_add(CALIBRATION_MAX_AGE.as_millis() as u64),
            operation_restore_start_percent: Some(restore_start_percent),
            operation_restore_end_percent: Some(restore_end_percent),
            ..Self::default()
        }
    }

    pub(crate) fn has_durable_operation(&self) -> bool {
        self.charge_once_active || self.operation != OperationKind::None
    }

    pub(crate) fn operation_expired(&self, now_unix_ms: u64) -> bool {
        self.operation != OperationKind::None
            && self.operation_expires_unix_ms != 0
            && now_unix_ms >= self.operation_expires_unix_ms
    }

    pub(crate) fn complete_calibration_restoration(
        &mut self,
        start: u8,
        end: u8,
        verified: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            self.operation == OperationKind::Calibration,
            "no calibration to restore"
        );
        anyhow::ensure!(
            verified
                && (Some(start), Some(end))
                    == (
                        self.operation_restore_start_percent,
                        self.operation_restore_end_percent
                    ),
            "battery {} reported thresholds {start}–{end} after calibration restoration; recovery remains pending",
            self.operation_battery_id
        );
        self.clear_operation();
        Ok(())
    }

    pub(crate) fn clear_operation(&mut self) {
        self.operation = OperationKind::None;
        self.operation_battery_id.clear();
        self.operation_phase = OperationPhase::None;
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
        .unwrap_or_else(|| data_file(XdgRoot::Config, "battery.json"))
}

pub(crate) fn state_path() -> PathBuf {
    env::var_os("BAR_DAEMON_BATTERY_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_file(XdgRoot::State, "battery-state.json"))
}

pub(crate) async fn load_config() -> Result<BatteryConfig> {
    let config: BatteryConfig = load_json_or_default(&config_path(), "battery data").await?;
    config.validate()?;
    Ok(config)
}

pub(crate) async fn save_config(config: &BatteryConfig) -> Result<()> {
    config.validate()?;
    save_json_atomic(&config_path(), config).await
}

pub(crate) async fn load_runtime() -> Result<BatteryRuntimeState> {
    load_json_or_default(&state_path(), "battery data").await
}

pub(crate) async fn save_runtime(state: &BatteryRuntimeState) -> Result<()> {
    save_json_atomic(&state_path(), state).await
}

#[cfg(test)]
mod restoration_tests {
    use super::*;

    #[test]
    fn restoration_marker_is_cleared_only_after_exact_verified_readback() {
        let mut runtime = BatteryRuntimeState::start_calibration(1, "BAT0".into(), 75, 80);
        runtime.operation_phase = OperationPhase::Restoring;
        let pending = runtime.clone();
        assert!(
            runtime
                .complete_calibration_restoration(0, 100, false)
                .is_err()
        );
        assert_eq!(runtime, pending);
        assert!(
            runtime
                .complete_calibration_restoration(75, 80, false)
                .is_err()
        );
        assert_eq!(runtime, pending);
        assert!(
            runtime
                .complete_calibration_restoration(0, 100, true)
                .is_err()
        );
        assert_eq!(runtime, pending);
        let serialized = serde_json::to_vec(&runtime).unwrap();
        runtime = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(runtime.operation_phase, OperationPhase::Restoring);
        runtime
            .complete_calibration_restoration(75, 80, true)
            .unwrap();
        assert!(!runtime.has_durable_operation());
        assert_eq!(runtime.operation_restore_start_percent, None);
    }
}

#[cfg(test)]
mod tests {
    use super::BatteryConfig;

    #[test]
    fn legacy_settings_preserve_thresholds_and_migrate_both_actions() {
        use crate::model::BatteryProfileAction;
        for enabled in [false, true] {
            let config: BatteryConfig = serde_json::from_value(serde_json::json!({
                "warning_percent": 35, "critical_percent": 8,
                "notify_when_full": false, "auto_power_saver": enabled
            }))
            .unwrap();
            let expected = if enabled {
                BatteryProfileAction::PowerSaver
            } else {
                BatteryProfileAction::KeepCurrent
            };
            assert_eq!(config.warning_profile(), expected);
            assert_eq!(config.critical_profile(), expected);
            assert!(config.notify_warning && config.notify_critical);
            assert!(!config.notify_when_full);
            assert_eq!((config.warning_percent, config.critical_percent), (35, 8));
            let roundtrip: BatteryConfig =
                serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
            assert_eq!(roundtrip, config);
        }
        assert!(
            serde_json::from_value::<BatteryConfig>(
                serde_json::json!({"warning_profile": "invalid"})
            )
            .is_err()
        );
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
                warning_percent: 100,
                critical_percent: 100,
                ..BatteryConfig::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            BatteryConfig {
                warning_percent: 101,
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
}
