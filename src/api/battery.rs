use serde_json::{Value, json};

use crate::battery;

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
        self.apply_battery_thresholds(battery_id, start, end).await
    }

    pub(super) async fn battery_set_protection(&self, params: Value) -> Value {
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error("validation-error", "battery.setProtection requires enabled");
        };
        let battery_id = match primary_battery_id(&self.state).await {
            Ok(id) => id,
            Err(response) => return response,
        };
        let (start, end) = if enabled { (75, 80) } else { (0, 100) };
        self.apply_battery_thresholds(&battery_id, start, end).await
    }

    pub(super) async fn battery_charge_once(&self) -> Value {
        let battery_id = match primary_battery_id(&self.state).await {
            Ok(id) => id,
            Err(response) => return response,
        };
        self.apply_battery_thresholds(&battery_id, 0, 100).await
    }

    async fn apply_battery_thresholds(&self, battery_id: &str, start: u8, end: u8) -> Value {
        let _guard = self.battery_effects.lock().await;
        match battery::helper::set_thresholds(battery_id, start, end).await {
            Ok((actual_start, actual_end)) => match battery::refresh_state(&self.state).await {
                Ok(state) => success(json!({
                    "battery": state,
                    "operation": {
                        "battery_id": battery_id,
                        "start_percent": actual_start,
                        "end_percent": actual_end
                    }
                })),
                Err(error_value) => error("battery-refresh-failed", error_value.to_string()),
            },
            Err(error_value) => error("battery-operation-failed", error_value.to_string()),
        }
    }
}

fn percent(params: &Value, name: &str) -> Option<u8> {
    let value = params.get(name)?.as_u64()?;
    (value <= 100).then(|| value as u8)
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
