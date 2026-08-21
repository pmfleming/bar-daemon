use serde_json::{Value, json};

use crate::battery::{self, config};

use super::{ApiService, error, success};

impl ApiService {
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
        let battery_id = match primary_battery_id(&self.state).await {
            Ok(id) => id,
            Err(response) => return response,
        };
        let _guard = battery::lock_effects().await;
        let previous_config = match config::load_config().await {
            Ok(config) => config,
            Err(error_value) => return error("battery-config-failed", error_value.to_string()),
        };
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

    pub(super) async fn battery_charge_once(&self) -> Value {
        let snapshot = self.state.snapshot().await.battery;
        if !snapshot.plugged {
            return error(
                "battery-not-plugged",
                "battery.chargeOnce requires external power",
            );
        }
        let Some(device) = snapshot.devices.first() else {
            return error(
                "battery-unavailable",
                "no controllable system battery is available",
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
