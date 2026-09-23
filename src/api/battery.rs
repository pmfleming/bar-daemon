use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    battery::{self, config},
    model::BatteryProfileAction,
    state::StateStore,
};

use super::{error, success};

#[derive(Deserialize)]
struct BatteryRequest {
    battery_id: Option<String>,
}

#[derive(Deserialize)]
struct ThresholdRequest {
    battery_id: String,
    start_percent: u8,
    end_percent: u8,
}

#[derive(Deserialize)]
struct ProtectionRequest {
    battery_id: Option<String>,
    enabled: bool,
    start_percent: Option<u8>,
    end_percent: Option<u8>,
}

#[derive(Deserialize)]
struct InhibitionRequest {
    battery_id: Option<String>,
    enabled: bool,
}

#[derive(Default, PartialEq, Deserialize)]
struct AlertPolicyRequest {
    warning_percent: Option<u8>,
    critical_percent: Option<u8>,
    notify_when_full: Option<bool>,
    auto_power_saver: Option<bool>,
    notify_warning: Option<bool>,
    notify_critical: Option<bool>,
    warning_profile: Option<BatteryProfileAction>,
    critical_profile: Option<BatteryProfileAction>,
}

impl AlertPolicyRequest {
    fn apply_to(self, config: &mut config::BatteryConfig) -> anyhow::Result<()> {
        anyhow::ensure!(
            self != Self::default(),
            "battery.setAlertPolicy requires at least one policy field"
        );
        config.warning_percent = self.warning_percent.unwrap_or(config.warning_percent);
        config.critical_percent = self.critical_percent.unwrap_or(config.critical_percent);
        config.notify_when_full = self.notify_when_full.unwrap_or(config.notify_when_full);
        // Resolve legacy defaults before applying partial updates.
        let mut warning = config.warning_profile();
        let mut critical = config.critical_profile();
        if let Some(value) = self.auto_power_saver {
            let action = if value {
                BatteryProfileAction::PowerSaver
            } else {
                BatteryProfileAction::KeepCurrent
            };
            warning = action;
            critical = action;
        }
        config.warning_profile = Some(self.warning_profile.unwrap_or(warning));
        config.critical_profile = Some(self.critical_profile.unwrap_or(critical));
        config.auto_power_saver = config.warning_profile().profile().is_some()
            || config.critical_profile().profile().is_some();
        config.notify_warning = self.notify_warning.unwrap_or(config.notify_warning);
        config.notify_critical = self.notify_critical.unwrap_or(config.notify_critical);
        config.validate()
    }
}

#[derive(Clone)]
pub(super) struct BatteryApi {
    state: StateStore,
}

impl BatteryApi {
    pub(super) fn new(state: StateStore) -> Self {
        Self { state }
    }

    pub(super) fn battery_history(&self) -> Value {
        success(json!({ "history": battery::history_snapshot() }))
    }

    pub(super) async fn battery_set_thresholds(&self, params: Value) -> Value {
        let request = request!(params, ThresholdRequest, "battery.setThresholds");
        if !valid_thresholds(request.start_percent, request.end_percent) {
            return error(
                "validation-error",
                "battery start_percent must be lower than end_percent",
            );
        }

        let _guard = battery::lock_effects().await;
        let previous_config = match config::load_config().await {
            Ok(config) => config,
            Err(error_value) => return error("battery-config-failed", error_value.to_string()),
        };
        let runtime = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if runtime.has_durable_operation() {
            return error(
                "battery-operation-active",
                "charge policy cannot change during a durable battery operation",
            );
        }
        let mut next_config = previous_config.clone();
        let protection_active = {
            let device = next_config.device_mut(&request.battery_id);
            device.protected_start_percent = request.start_percent;
            device.protected_end_percent = request.end_percent;
            let active = device.manage_thresholds && device.protection_enabled;
            if active {
                device.accepted_reported_start_percent = None;
                device.accepted_reported_end_percent = None;
            }
            active
        };
        if protection_active {
            self.apply_policy_change(
                &request.battery_id,
                request.start_percent,
                request.end_percent,
                previous_config,
                next_config,
            )
            .await
        } else {
            self.save_policy_change(next_config).await
        }
    }

    pub(super) async fn battery_set_protection(&self, params: Value) -> Value {
        let request = request!(params, ProtectionRequest, "battery.setProtection");
        let requested_thresholds =
            match optional_thresholds(request.start_percent, request.end_percent) {
                Ok(value) => value,
                Err(message) => return error("validation-error", message),
            };
        let _guard = battery::lock_effects().await;
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
        let previous_config = match config::load_config().await {
            Ok(config) => config,
            Err(error_value) => return error("battery-config-failed", error_value.to_string()),
        };
        let runtime = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if runtime.has_durable_operation() {
            return error(
                "battery-operation-active",
                "charge policy cannot change during a durable battery operation",
            );
        }
        let mut next_config = previous_config.clone();
        let device = next_config.device_mut(&battery_id);
        if let Some((start, end)) = requested_thresholds {
            device.protected_start_percent = start;
            device.protected_end_percent = end;
        }
        let (start, end) = if request.enabled {
            (device.protected_start_percent, device.protected_end_percent)
        } else {
            (0, 100)
        };
        device.manage_thresholds = true;
        device.protection_enabled = request.enabled;
        device.accepted_reported_start_percent = None;
        device.accepted_reported_end_percent = None;
        self.apply_policy_change(&battery_id, start, end, previous_config, next_config)
            .await
    }

    pub(super) async fn battery_charge_once(&self, params: Value) -> Value {
        let request = request!(params, BatteryRequest, "battery.chargeOnce");
        let _guard = battery::lock_effects().await;
        let snapshot = self.state.snapshot().await.battery;
        if !snapshot.plugged {
            return error(
                "battery-not-plugged",
                "battery.chargeOnce requires external power",
            );
        }
        let requested_id = request.battery_id.as_deref();
        let device = requested_id
            .and_then(|id| snapshot.devices.iter().find(|device| device.id == id))
            .or_else(|| {
                requested_id
                    .is_none()
                    .then(|| snapshot.devices.first())
                    .flatten()
            });
        let Some(device) = device else {
            return error(
                "battery-unavailable",
                requested_id.map_or_else(
                    || "no controllable system battery is available".into(),
                    |id| format!("battery {id} is unavailable"),
                ),
            );
        };
        let battery_id = device.id.clone();
        let (Some(restore_start), Some(restore_end)) = (
            device.protection.start_percent,
            device.protection.end_percent,
        ) else {
            return error(
                "battery-protection-unsupported",
                "battery.chargeOnce requires readable charge thresholds",
            );
        };
        let previous_runtime = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if previous_runtime.has_durable_operation() {
            return error(
                "battery-operation-active",
                "another durable battery operation is already active",
            );
        }
        let runtime = config::BatteryRuntimeState::start_charge_once(
            crate::time::unix_ms(),
            battery_id.clone(),
            restore_start,
            restore_end,
        );
        if let Err(error_value) = config::save_runtime(&runtime).await {
            return error("battery-state-failed", error_value.to_string());
        }
        match battery::helper::set_thresholds(&battery_id, 0, 100).await {
            Ok(result) => self.battery_response(Some((&battery_id, result))).await,
            Err(error_value) => {
                if let Err(rollback_error) = config::save_runtime(&previous_runtime).await {
                    return error(
                        "battery-operation-failed",
                        format!(
                            "{error_value}; charge-once state rollback also failed: {rollback_error}"
                        ),
                    );
                }
                error("battery-operation-failed", error_value.to_string())
            }
        }
    }

    pub(super) async fn battery_set_charging_inhibited(&self, params: Value) -> Value {
        let request = request!(params, InhibitionRequest, "battery.setChargingInhibited");
        let _guard = battery::lock_effects().await;
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
        let snapshot = self.state.snapshot().await.battery;
        if let Err(kind) =
            battery::require_device_behaviour(&snapshot, &battery_id, "inhibit-charge")
        {
            return capability_error(kind, &battery_id, "inhibit charging");
        }
        let previous = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        let result = if request.enabled {
            start_charging_inhibition(&battery_id, &previous).await
        } else {
            stop_charging_inhibition(&battery_id, previous).await
        };
        match result {
            Ok(()) => self.battery_response(None).await,
            Err(response) => response,
        }
    }

    pub(super) async fn battery_start_calibration(&self, params: Value) -> Value {
        let request = request!(params, BatteryRequest, "battery.startCalibration");
        let _guard = battery::lock_effects().await;
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
        let snapshot = self.state.snapshot().await.battery;
        if !snapshot.plugged {
            return error(
                "battery-not-plugged",
                "battery calibration requires external power",
            );
        }
        let device =
            match battery::require_device_behaviour(&snapshot, &battery_id, "force-discharge") {
                Ok(device) => device,
                Err(kind) => return capability_error(kind, &battery_id, "force discharge"),
            };
        let (Some(restore_start), Some(restore_end)) = (
            device.protection.start_percent,
            device.protection.end_percent,
        ) else {
            return error(
                "battery-protection-unsupported",
                "battery calibration requires readable charge thresholds",
            );
        };
        let previous = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if let Err(response) =
            begin_calibration(&battery_id, restore_start, restore_end, &previous).await
        {
            return response;
        }
        self.battery_response(None).await
    }

    pub(super) async fn battery_cancel_calibration(&self, params: Value) -> Value {
        let request = request!(params, BatteryRequest, "battery.cancelCalibration");
        let _guard = battery::lock_effects().await;
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
        let mut runtime = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if runtime.operation != config::OperationKind::Calibration
            || runtime.operation_battery_id != battery_id
        {
            return error(
                "battery-operation-inactive",
                format!("battery {battery_id} is not being calibrated"),
            );
        }
        // Persist cancellation before touching hardware. A failed restoration
        // must retry restoration on restart, never resume force-discharge.
        runtime.operation_phase = config::OperationPhase::Restoring;
        if let Err(error_value) = config::save_runtime(&runtime).await {
            return error("battery-state-failed", error_value.to_string());
        }
        if let Err(error_value) = battery::helper::set_charge_behaviour(&battery_id, "auto").await {
            return error("battery-operation-failed", error_value.to_string());
        }
        let (Some(start), Some(end)) = (
            runtime.operation_restore_start_percent,
            runtime.operation_restore_end_percent,
        ) else {
            return error(
                "battery-state-failed",
                "calibration restoration thresholds are missing",
            );
        };
        let result = match battery::helper::set_thresholds(&battery_id, start, end).await {
            Ok(result) => result,
            Err(error_value) => return error("battery-operation-failed", error_value.to_string()),
        };
        if let Err(error_value) = runtime.complete_calibration_restoration(
            result.actual_start_percent,
            result.actual_end_percent,
            result.verified,
        ) {
            return error("battery-operation-failed", error_value.to_string());
        }
        if let Err(error_value) = config::save_runtime(&runtime).await {
            return error("battery-state-failed", error_value.to_string());
        }
        self.battery_response(None).await
    }

    pub(super) async fn battery_set_alert_policy(&self, params: Value) -> Value {
        let request = request!(params, AlertPolicyRequest, "battery.setAlertPolicy");
        let _guard = battery::lock_effects().await;
        let mut next_config = match config::load_config().await {
            Ok(config) => config,
            Err(error_value) => return error("battery-config-failed", error_value.to_string()),
        };
        let requested_actions = [
            request
                .warning_profile
                .filter(|action| *action != next_config.warning_profile()),
            request
                .critical_profile
                .filter(|action| *action != next_config.critical_profile()),
        ];
        if let Err(error_value) = request.apply_to(&mut next_config) {
            return error("validation-error", error_value.to_string());
        }
        // Keep-current is always valid, even if the profile service is offline.
        let profiles = self.state.snapshot().await.power_profile;
        for action in requested_actions.into_iter().flatten() {
            if let Some(profile) = action.profile()
                && profiles.available
                && !profiles.profiles.iter().any(|item| item.name == profile)
            {
                return error(
                    "validation-error",
                    format!("power profile is unavailable: {profile}"),
                );
            }
        }
        if let Err(error_value) = config::save_config(&next_config).await {
            return error("battery-config-failed", error_value.to_string());
        }
        match battery::refresh_state(&self.state).await {
            Ok(state) => success(json!({ "battery": state })),
            Err(error_value) => error("battery-refresh-failed", error_value.to_string()),
        }
    }

    async fn save_policy_change(&self, next_config: config::BatteryConfig) -> Value {
        if let Err(error_value) = config::save_config(&next_config).await {
            return error("battery-config-failed", error_value.to_string());
        }
        self.battery_response(None).await
    }

    async fn apply_policy_change(
        &self,
        battery_id: &str,
        start: u8,
        end: u8,
        previous_config: config::BatteryConfig,
        mut next_config: config::BatteryConfig,
    ) -> Value {
        if let Err(error_value) = config::save_config(&next_config).await {
            return error("battery-config-failed", error_value.to_string());
        }
        match battery::helper::set_thresholds(battery_id, start, end).await {
            Ok(result) => {
                let device = next_config.device_mut(battery_id);
                device.accepted_reported_start_percent = Some(result.actual_start_percent);
                device.accepted_reported_end_percent = Some(result.actual_end_percent);
                if let Err(error_value) = config::save_config(&next_config).await {
                    return error("battery-config-failed", error_value.to_string());
                }
                self.battery_response(Some((battery_id, result))).await
            }
            Err(error_value) => {
                if let Err(rollback_error) = config::save_config(&previous_config).await {
                    return error(
                        "battery-operation-failed",
                        format!(
                            "{error_value}; battery configuration rollback also failed: {rollback_error}"
                        ),
                    );
                }
                error("battery-operation-failed", error_value.to_string())
            }
        }
    }

    async fn battery_response(
        &self,
        operation: Option<(&str, battery::helper::ThresholdWriteResult)>,
    ) -> Value {
        match battery::refresh_state(&self.state).await {
            Ok(state) => {
                let mut data = json!({ "battery": state });
                if let Some((battery_id, result)) = operation {
                    data["operation"] = json!({
                        "battery_id": battery_id,
                        "start_percent": result.actual_start_percent,
                        "end_percent": result.actual_end_percent,
                        "verified": result.verified
                    });
                }
                success(data)
            }
            Err(error_value) => error("battery-refresh-failed", error_value.to_string()),
        }
    }
}

fn capability_error(kind: battery::DeviceSupportError, battery_id: &str, action: &str) -> Value {
    match kind {
        battery::DeviceSupportError::Missing => error(
            "battery-unavailable",
            format!("battery {battery_id} is unavailable"),
        ),
        battery::DeviceSupportError::Unsupported => error(
            "battery-operation-unsupported",
            format!("battery {battery_id} cannot {action}"),
        ),
    }
}

fn valid_thresholds(start: u8, end: u8) -> bool {
    start < end && end <= 100
}

fn optional_thresholds(
    start: Option<u8>,
    end: Option<u8>,
) -> Result<Option<(u8, u8)>, &'static str> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) if valid_thresholds(start, end) => Ok(Some((start, end))),
        (Some(_), Some(_)) => Err("battery start_percent must be lower than end_percent"),
        _ => Err("battery start_percent and end_percent must be supplied together"),
    }
}

async fn begin_calibration(
    battery_id: &str,
    restore_start: u8,
    restore_end: u8,
    previous: &config::BatteryRuntimeState,
) -> Result<(), Value> {
    if previous.has_durable_operation() {
        return Err(error(
            "battery-operation-active",
            "another durable battery operation is already active",
        ));
    }
    let next = config::BatteryRuntimeState::start_calibration(
        crate::time::unix_ms(),
        battery_id.to_string(),
        restore_start,
        restore_end,
    );
    config::save_runtime(&next)
        .await
        .map_err(|value| error("battery-state-failed", value.to_string()))?;
    if let Err(value) = battery::helper::set_thresholds(battery_id, 0, 100).await {
        return Err(rollback_calibration_state(previous, value).await);
    }
    if let Err(value) = battery::helper::set_charge_behaviour(battery_id, "force-discharge").await {
        return Err(rollback_calibration_hardware(
            battery_id,
            restore_start,
            restore_end,
            previous,
            value,
        )
        .await);
    }
    Ok(())
}

async fn rollback_calibration_state(
    previous: &config::BatteryRuntimeState,
    failure: impl std::fmt::Display,
) -> Value {
    match config::save_runtime(previous).await {
        Ok(()) => error("battery-operation-failed", failure.to_string()),
        Err(rollback_error) => error(
            "battery-operation-failed",
            format!("{failure}; calibration state rollback also failed: {rollback_error}"),
        ),
    }
}

async fn rollback_calibration_hardware(
    battery_id: &str,
    restore_start: u8,
    restore_end: u8,
    previous: &config::BatteryRuntimeState,
    failure: impl std::fmt::Display,
) -> Value {
    let mut message = failure.to_string();
    match battery::helper::set_thresholds(battery_id, restore_start, restore_end).await {
        Ok(result) if !result.verified => message.push_str(&format!(
            "; threshold rollback reported {}–{} instead of {restore_start}–{restore_end}",
            result.actual_start_percent, result.actual_end_percent
        )),
        Ok(_) => {}
        Err(error) => message.push_str(&format!("; threshold rollback also failed: {error}")),
    }
    if let Err(error) = config::save_runtime(previous).await {
        message.push_str(&format!(
            "; calibration state rollback also failed: {error}"
        ));
    }
    error("battery-operation-failed", message)
}

async fn start_charging_inhibition(
    battery_id: &str,
    previous: &config::BatteryRuntimeState,
) -> Result<(), Value> {
    if previous.has_durable_operation() {
        return Err(error(
            "battery-operation-active",
            "another durable battery operation is already active",
        ));
    }
    let next =
        config::BatteryRuntimeState::start_inhibit(crate::time::unix_ms(), battery_id.to_string());
    config::save_runtime(&next)
        .await
        .map_err(|value| error("battery-state-failed", value.to_string()))?;
    if let Err(value) = battery::helper::set_charge_behaviour(battery_id, "inhibit-charge").await {
        return match config::save_runtime(previous).await {
            Ok(()) => Err(error("battery-operation-failed", value.to_string())),
            Err(rollback_error) => Err(error(
                "battery-operation-failed",
                format!("{value}; inhibition state rollback also failed: {rollback_error}"),
            )),
        };
    }
    Ok(())
}

async fn stop_charging_inhibition(
    battery_id: &str,
    mut runtime: config::BatteryRuntimeState,
) -> Result<(), Value> {
    if runtime.operation != config::OperationKind::Inhibit
        || runtime.operation_battery_id != battery_id
    {
        return Err(error(
            "battery-operation-inactive",
            format!("charging is not durably inhibited for {battery_id}"),
        ));
    }
    battery::helper::set_charge_behaviour(battery_id, "auto")
        .await
        .map_err(|value| error("battery-operation-failed", value.to_string()))?;
    runtime.clear_operation();
    config::save_runtime(&runtime)
        .await
        .map_err(|value| error("battery-state-failed", value.to_string()))
}

async fn primary_battery_id(state: &crate::state::StateStore) -> Result<String, Value> {
    state
        .snapshot()
        .await
        .battery
        .devices
        .first()
        .map(|device| device.id.clone())
        .ok_or_else(|| {
            error(
                "battery-unavailable",
                "no controllable system battery is available",
            )
        })
}

async fn requested_battery_id(
    requested: Option<&str>,
    state: &crate::state::StateStore,
) -> Result<String, Value> {
    let Some(battery_id) = requested else {
        return primary_battery_id(state).await;
    };
    let available = state
        .snapshot()
        .await
        .battery
        .devices
        .iter()
        .any(|device| device.id == battery_id);
    available.then(|| battery_id.to_string()).ok_or_else(|| {
        error(
            "battery-unavailable",
            format!("battery {battery_id} is unavailable"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{AlertPolicyRequest, ProtectionRequest, optional_thresholds};
    use crate::{battery::config::BatteryConfig, model::BatteryProfileAction};

    #[test]
    fn level_policy_partial_updates_preserve_independent_actions() {
        let mut config = BatteryConfig {
            auto_power_saver: false,
            ..Default::default()
        };
        let request: AlertPolicyRequest = serde_json::from_value(serde_json::json!({
            "warning_profile": "balanced", "notify_warning": false, "notify_when_full": false
        }))
        .unwrap();
        request.apply_to(&mut config).unwrap();
        assert_eq!(config.warning_profile(), BatteryProfileAction::Balanced);
        assert_eq!(config.critical_profile(), BatteryProfileAction::KeepCurrent);
        assert!(!config.notify_warning && config.notify_critical && !config.notify_when_full);
        let request: AlertPolicyRequest =
            serde_json::from_value(serde_json::json!({"notify_critical": false})).unwrap();
        request.apply_to(&mut config).unwrap();
        assert_eq!(config.warning_profile(), BatteryProfileAction::Balanced);
        assert_eq!(config.critical_profile(), BatteryProfileAction::KeepCurrent);
        assert!(!config.notify_warning && !config.notify_critical);
        let request: AlertPolicyRequest = serde_json::from_value(serde_json::json!({
            "warning_profile": "keep-current", "critical_profile": "performance"
        }))
        .unwrap();
        request.apply_to(&mut config).unwrap();
        assert_eq!(config.warning_profile(), BatteryProfileAction::KeepCurrent);
        assert_eq!(config.critical_profile(), BatteryProfileAction::Performance);
        assert!(
            serde_json::from_value::<AlertPolicyRequest>(
                serde_json::json!({"warning_profile": "turbo"})
            )
            .is_err()
        );
        for value in [
            serde_json::json!({}),
            serde_json::json!({"warning_percent": 5, "critical_percent": 10}),
        ] {
            let request: AlertPolicyRequest = serde_json::from_value(value).unwrap();
            assert!(request.apply_to(&mut config).is_err());
        }
    }

    #[test]
    fn protection_accepts_an_atomic_optional_range() {
        let request: ProtectionRequest = serde_json::from_str(
            r#"{"battery_id":"BAT0","enabled":true,"start_percent":70,"end_percent":85}"#,
        )
        .unwrap();
        assert_eq!(
            optional_thresholds(request.start_percent, request.end_percent),
            Ok(Some((70, 85)))
        );
        assert!(optional_thresholds(Some(80), Some(80)).is_err());
        assert!(optional_thresholds(Some(70), None).is_err());
        assert_eq!(optional_thresholds(None, None), Ok(None));
    }
}
