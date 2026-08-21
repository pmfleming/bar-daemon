use serde_json::{Value, json};

use crate::battery::{self, config};

use super::{ApiService, error, success};

impl ApiService {
    pub(super) fn battery_history(&self) -> Value {
        success(json!({ "history": battery::history_snapshot() }))
    }

    pub(super) async fn battery_set_thresholds(&self, params: Value) -> Value {
        let Some(battery_id) = params.get("battery_id").and_then(Value::as_str) else {
            return error(
                "validation-error",
                "battery.setThresholds requires battery_id",
            );
        };
        let Some(start) = percent(&params, "start_percent") else {
            return error(
                "validation-error",
                "battery.setThresholds requires start_percent from 0 to 99",
            );
        };
        let Some(end) = percent(&params, "end_percent") else {
            return error(
                "validation-error",
                "battery.setThresholds requires end_percent from 1 to 100",
            );
        };
        if start >= end {
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
        if runtime.charge_once_active || !runtime.operation.is_empty() {
            return error(
                "battery-operation-active",
                "charge policy cannot change during a durable battery operation",
            );
        }
        let mut next_config = previous_config.clone();
        let device = next_config.device_mut(battery_id);
        device.protected_start_percent = start;
        device.protected_end_percent = end;
        device.manage_thresholds = true;
        device.protection_enabled = true;
        device.accepted_reported_start_percent = None;
        device.accepted_reported_end_percent = None;
        self.apply_policy_change(battery_id, start, end, previous_config, next_config)
            .await
    }

    pub(super) async fn battery_set_protection(&self, params: Value) -> Value {
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error("validation-error", "battery.setProtection requires enabled");
        };
        let battery_id = match requested_battery_id(&params, &self.state).await {
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
        if runtime.charge_once_active || !runtime.operation.is_empty() {
            return error(
                "battery-operation-active",
                "charge policy cannot change during a durable battery operation",
            );
        }
        let mut next_config = previous_config.clone();
        let device = next_config.device_mut(&battery_id);
        let (start, end) = if enabled {
            (device.protected_start_percent, device.protected_end_percent)
        } else {
            (0, 100)
        };
        device.manage_thresholds = true;
        device.protection_enabled = enabled;
        device.accepted_reported_start_percent = None;
        device.accepted_reported_end_percent = None;
        self.apply_policy_change(&battery_id, start, end, previous_config, next_config)
            .await
    }

    pub(super) async fn battery_charge_once(&self, params: Value) -> Value {
        let snapshot = self.state.snapshot().await.battery;
        if !snapshot.plugged {
            return error(
                "battery-not-plugged",
                "battery.chargeOnce requires external power",
            );
        }
        let requested_id = params.get("battery_id").and_then(Value::as_str);
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
        if previous_runtime.charge_once_active || !previous_runtime.operation.is_empty() {
            return error(
                "battery-operation-active",
                "another durable battery operation is already active",
            );
        }
        let runtime = config::BatteryRuntimeState::start_charge_once(
            config::unix_ms(),
            battery_id.clone(),
            restore_start,
            restore_end,
        );
        if let Err(error_value) = config::save_runtime(&runtime).await {
            return error("battery-state-failed", error_value.to_string());
        }
        match battery::helper::set_thresholds(&battery_id, 0, 100).await {
            Ok(result) => self.battery_operation_response(&battery_id, result).await,
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
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error(
                "validation-error",
                "battery.setChargingInhibited requires enabled",
            );
        };
        let battery_id = match requested_battery_id(&params, &self.state).await {
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
        if enabled {
            if previous.charge_once_active || !previous.operation.is_empty() {
                return error(
                    "battery-operation-active",
                    "another durable battery operation is already active",
                );
            }
            let next =
                config::BatteryRuntimeState::start_inhibit(config::unix_ms(), battery_id.clone());
            if let Err(error_value) = config::save_runtime(&next).await {
                return error("battery-state-failed", error_value.to_string());
            }
            if let Err(error_value) =
                battery::helper::set_charge_behaviour(&battery_id, "inhibit-charge").await
            {
                let _ = config::save_runtime(&previous).await;
                return error("battery-operation-failed", error_value.to_string());
            }
        } else {
            if previous.operation != "inhibit" || previous.operation_battery_id != battery_id {
                return error(
                    "battery-operation-inactive",
                    format!("charging is not durably inhibited for {battery_id}"),
                );
            }
            if let Err(error_value) =
                battery::helper::set_charge_behaviour(&battery_id, "auto").await
            {
                return error("battery-operation-failed", error_value.to_string());
            }
            let mut next = previous;
            next.clear_operation();
            if let Err(error_value) = config::save_runtime(&next).await {
                return error("battery-state-failed", error_value.to_string());
            }
        }
        self.battery_refresh_response().await
    }

    pub(super) async fn battery_start_calibration(&self, params: Value) -> Value {
        let battery_id = match requested_battery_id(&params, &self.state).await {
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
        if previous.charge_once_active || !previous.operation.is_empty() {
            return error(
                "battery-operation-active",
                "another durable battery operation is already active",
            );
        }
        let next = config::BatteryRuntimeState::start_calibration(
            config::unix_ms(),
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
        self.battery_refresh_response().await
    }

    pub(super) async fn battery_cancel_calibration(&self, params: Value) -> Value {
        let battery_id = match requested_battery_id(&params, &self.state).await {
            Ok(id) => id,
            Err(response) => return response,
        };
        let _guard = battery::lock_effects().await;
        let mut runtime = match config::load_runtime().await {
            Ok(runtime) => runtime,
            Err(error_value) => return error("battery-state-failed", error_value.to_string()),
        };
        if runtime.operation != "calibration" || runtime.operation_battery_id != battery_id {
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
        self.battery_refresh_response().await
    }

    async fn battery_refresh_response(&self) -> Value {
        match battery::refresh_state(&self.state).await {
            Ok(state) => success(json!({ "battery": state })),
            Err(error_value) => error("battery-refresh-failed", error_value.to_string()),
        }
    }

    pub(super) async fn battery_set_alert_policy(&self, params: Value) -> Value {
        let _guard = battery::lock_effects().await;
        let mut next_config = match config::load_config().await {
            Ok(config) => config,
            Err(error_value) => return error("battery-config-failed", error_value.to_string()),
        };
        let mut changed = false;
        if params.get("warning_percent").is_some() {
            let Some(value) = percent(&params, "warning_percent") else {
                return error("validation-error", "warning_percent must be from 0 to 100");
            };
            next_config.warning_percent = value;
            changed = true;
        }
        if params.get("critical_percent").is_some() {
            let Some(value) = percent(&params, "critical_percent") else {
                return error("validation-error", "critical_percent must be from 0 to 100");
            };
            next_config.critical_percent = value;
            changed = true;
        }
        if params.get("notify_when_full").is_some() {
            let Some(value) = params.get("notify_when_full").and_then(Value::as_bool) else {
                return error("validation-error", "notify_when_full must be boolean");
            };
            next_config.notify_when_full = value;
            changed = true;
        }
        if params.get("auto_power_saver").is_some() {
            let Some(value) = params.get("auto_power_saver").and_then(Value::as_bool) else {
                return error("validation-error", "auto_power_saver must be boolean");
            };
            next_config.auto_power_saver = value;
            changed = true;
        }
        if !changed {
            return error(
                "validation-error",
                "battery.setAlertPolicy requires at least one policy field",
            );
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
                self.battery_operation_response(battery_id, result).await
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

    async fn battery_operation_response(
        &self,
        battery_id: &str,
        result: battery::helper::ThresholdWriteResult,
    ) -> Value {
        match battery::refresh_state(&self.state).await {
            Ok(state) => success(json!({
                "battery": state,
                "operation": {
                    "battery_id": battery_id,
                    "start_percent": result.actual_start_percent,
                    "end_percent": result.actual_end_percent,
                    "verified": result.verified
                }
            })),
            Err(error_value) => error("battery-refresh-failed", error_value.to_string()),
        }
    }
}

fn percent(params: &Value, name: &str) -> Option<u8> {
    let value = params.get(name)?.as_u64()?;
    (value <= 100).then_some(value as u8)
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
    params: &Value,
    state: &crate::state::StateStore,
) -> Result<String, Value> {
    if let Some(value) = params.get("battery_id") {
        let Some(battery_id) = value.as_str() else {
            return Err(error("validation-error", "battery_id must be a string"));
        };
        let available = state
            .snapshot()
            .await
            .battery
            .devices
            .iter()
            .any(|device| device.id == battery_id);
        return available.then(|| battery_id.to_string()).ok_or_else(|| {
            error(
                "battery-unavailable",
                format!("battery {battery_id} is unavailable"),
            )
        });
    }
    primary_battery_id(state).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::percent;

    #[test]
    fn parses_bounded_percentages() {
        assert_eq!(percent(&json!({"value": 80}), "value"), Some(80));
        assert_eq!(percent(&json!({"value": 101}), "value"), None);
        assert_eq!(percent(&json!({"value": -1}), "value"), None);
    }
}
