use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    battery::{self, config},
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
}

#[derive(Deserialize)]
struct InhibitionRequest {
    battery_id: Option<String>,
    enabled: bool,
}

#[derive(Deserialize)]
struct AlertPolicyRequest {
    warning_percent: Option<u8>,
    critical_percent: Option<u8>,
    notify_when_full: Option<bool>,
    auto_power_saver: Option<bool>,
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
        if request.start_percent >= request.end_percent || request.end_percent > 100 {
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
        let device = next_config.device_mut(&request.battery_id);
        device.protected_start_percent = request.start_percent;
        device.protected_end_percent = request.end_percent;
        device.manage_thresholds = true;
        device.protection_enabled = true;
        device.accepted_reported_start_percent = None;
        device.accepted_reported_end_percent = None;
        self.apply_policy_change(
            &request.battery_id,
            request.start_percent,
            request.end_percent,
            previous_config,
            next_config,
        )
        .await
    }

    pub(super) async fn battery_set_protection(&self, params: Value) -> Value {
        let request = request!(params, ProtectionRequest, "battery.setProtection");
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
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
        let device = next_config.device_mut(&battery_id);
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
        let _guard = battery::lock_effects().await;
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
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
        let snapshot = self.state.snapshot().await.battery;
        let Some(device) = snapshot
            .devices
            .iter()
            .find(|device| device.id == battery_id)
        else {
            return error(
                "battery-unavailable",
                format!("battery {battery_id} is unavailable"),
            );
        };
        if !device
            .protection
            .available_behaviours
            .iter()
            .any(|value| value == "inhibit-charge")
        {
            return error(
                "battery-operation-unsupported",
                format!("battery {battery_id} cannot inhibit charging"),
            );
        }
        let _guard = battery::lock_effects().await;
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
        let Some(device) = snapshot
            .devices
            .iter()
            .find(|device| device.id == battery_id)
        else {
            return error(
                "battery-unavailable",
                format!("battery {battery_id} is unavailable"),
            );
        };
        if !device
            .protection
            .available_behaviours
            .iter()
            .any(|value| value == "force-discharge")
        {
            return error(
                "battery-operation-unsupported",
                format!("battery {battery_id} cannot force discharge"),
            );
        }
        let (Some(restore_start), Some(restore_end)) = (
            device.protection.start_percent,
            device.protection.end_percent,
        ) else {
            return error(
                "battery-protection-unsupported",
                "battery calibration requires readable charge thresholds",
            );
        };
        let _guard = battery::lock_effects().await;
        let previous = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if previous.has_durable_operation() {
            return error(
                "battery-operation-active",
                "another durable battery operation is already active",
            );
        }
        let next = config::BatteryRuntimeState::start_calibration(
            crate::time::unix_ms(),
            battery_id.clone(),
            restore_start,
            restore_end,
        );
        if let Err(error_value) = config::save_runtime(&next).await {
            return error("battery-state-failed", error_value.to_string());
        }
        if let Err(error_value) = battery::helper::set_thresholds(&battery_id, 0, 100).await {
            let _ = config::save_runtime(&previous).await;
            return error("battery-operation-failed", error_value.to_string());
        }
        if let Err(error_value) =
            battery::helper::set_charge_behaviour(&battery_id, "force-discharge").await
        {
            let restore_result =
                battery::helper::set_thresholds(&battery_id, restore_start, restore_end).await;
            let _ = config::save_runtime(&previous).await;
            return error(
                "battery-operation-failed",
                match restore_result {
                    Ok(_) => error_value.to_string(),
                    Err(restore_error) => {
                        format!("{error_value}; threshold rollback also failed: {restore_error}")
                    }
                },
            );
        }
        self.battery_response(None).await
    }

    pub(super) async fn battery_cancel_calibration(&self, params: Value) -> Value {
        let request = request!(params, BatteryRequest, "battery.cancelCalibration");
        let battery_id =
            match requested_battery_id(request.battery_id.as_deref(), &self.state).await {
                Ok(id) => id,
                Err(response) => return response,
            };
        let _guard = battery::lock_effects().await;
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
        if let Err(error_value) = battery::helper::set_charge_behaviour(&battery_id, "auto").await {
            return error("battery-operation-failed", error_value.to_string());
        }
        if let (Some(start), Some(end)) = (
            runtime.operation_restore_start_percent,
            runtime.operation_restore_end_percent,
        ) && let Err(error_value) =
            battery::helper::set_thresholds(&battery_id, start, end).await
        {
            return error("battery-operation-failed", error_value.to_string());
        }
        runtime.clear_operation();
        if let Err(error_value) = config::save_runtime(&runtime).await {
            return error("battery-state-failed", error_value.to_string());
        }
        self.battery_response(None).await
    }

    pub(super) async fn battery_set_alert_policy(&self, params: Value) -> Value {
        let request = request!(params, AlertPolicyRequest, "battery.setAlertPolicy");
        let changed = request.warning_percent.is_some()
            || request.critical_percent.is_some()
            || request.notify_when_full.is_some()
            || request.auto_power_saver.is_some();
        if !changed {
            return error(
                "validation-error",
                "battery.setAlertPolicy requires at least one policy field",
            );
        }
        let _guard = battery::lock_effects().await;
        let mut next_config = match config::load_config().await {
            Ok(config) => config,
            Err(error_value) => return error("battery-config-failed", error_value.to_string()),
        };
        if let Some(value) = request.warning_percent {
            next_config.warning_percent = value;
        }
        if let Some(value) = request.critical_percent {
            next_config.critical_percent = value;
        }
        if let Some(value) = request.notify_when_full {
            next_config.notify_when_full = value;
        }
        if let Some(value) = request.auto_power_saver {
            next_config.auto_power_saver = value;
        }
        if let Err(error_value) = next_config.validate() {
            return error("validation-error", error_value.to_string());
        }
        if let Err(error_value) = config::save_config(&next_config).await {
            return error("battery-config-failed", error_value.to_string());
        }
        match battery::refresh_state(&self.state).await {
            Ok(state) => success(json!({ "battery": state })),
            Err(error_value) => error("battery-refresh-failed", error_value.to_string()),
        }
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
        let _ = config::save_runtime(previous).await;
        return Err(error("battery-operation-failed", value.to_string()));
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
    use super::ThresholdRequest;

    #[test]
    fn parses_typed_thresholds() {
        let request: ThresholdRequest =
            serde_json::from_str(r#"{"battery_id":"BAT0","start_percent":75,"end_percent":80}"#)
                .unwrap();
        assert_eq!(request.start_percent, 75);
        assert!(
            serde_json::from_str::<ThresholdRequest>(
                r#"{"battery_id":"BAT0","start_percent":-1,"end_percent":80}"#
            )
            .is_err()
        );
    }
}
