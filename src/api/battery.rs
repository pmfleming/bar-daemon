use serde_json::Value;

use super::decode_request;

use crate::{
    battery::{
        self,
        config::{self, valid_thresholds},
    },
    model::BatteryProfileAction,
    state::StateStore,
};

use crate::protocol::battery_wire::{
    self, AlertPolicyRequest, BatteryRequest, ErrorCode, InhibitionRequest, ProtectionRequest,
    ThresholdRequest, error,
};

type Response = Result<Value, Value>;

impl AlertPolicyRequest {
    pub(crate) fn apply_to(self, config: &mut config::BatteryConfig) -> anyhow::Result<()> {
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
        config.auto_power_saver = config.policy().auto_power_saver;
        config.notify_warning = self.notify_warning.unwrap_or(config.notify_warning);
        config.notify_critical = self.notify_critical.unwrap_or(config.notify_critical);
        config.validate()
    }
}

pub(super) struct BatteryApi {
    state: StateStore,
}

impl BatteryApi {
    pub(super) fn new(state: StateStore) -> Self {
        Self { state }
    }

    pub(super) async fn dispatch(&self, method: &str, params: Value) -> Value {
        let result = match method {
            "battery.history" => Ok(battery_wire::history(battery::history_snapshot())),
            "battery.setThresholds" => self.battery_set_thresholds(params).await,
            "battery.setProtection" => self.battery_set_protection(params).await,
            "battery.chargeOnce" => self.battery_charge_once(params).await,
            "battery.setChargingInhibited" => self.battery_set_charging_inhibited(params).await,
            "battery.startCalibration" => self.battery_start_calibration(params).await,
            "battery.cancelCalibration" => self.battery_cancel_calibration(params).await,
            "battery.setAlertPolicy" => self.battery_set_alert_policy(params).await,
            _ => Err(super::unsupported_method(method)),
        };
        result.unwrap_or_else(|response| response)
    }

    async fn battery_set_thresholds(&self, params: Value) -> Response {
        let request: ThresholdRequest = decode_request(params, "battery.setThresholds")?;
        if !valid_thresholds(request.start_percent, request.end_percent) {
            return Err(error(
                ErrorCode::ValidationError,
                "battery start_percent must be lower than end_percent",
            ));
        }

        let _guard = battery::lock_effects().await;
        let previous_config = editable_policy().await?;
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

    async fn battery_set_protection(&self, params: Value) -> Response {
        let request: ProtectionRequest = decode_request(params, "battery.setProtection")?;
        let requested_thresholds = optional_thresholds(request.start_percent, request.end_percent)
            .map_err(|message| error(ErrorCode::ValidationError, message))?;
        let _guard = battery::lock_effects().await;
        let battery_id = requested_battery_id(request.battery_id.as_deref(), &self.state).await?;
        let previous_config = editable_policy().await?;
        let mut next_config = previous_config.clone();
        let device = next_config.device_mut(&battery_id);
        if let Some((start, end)) = requested_thresholds {
            device.protected_start_percent = start;
            device.protected_end_percent = end;
        }
        device.manage_thresholds = true;
        device.protection_enabled = request.enabled;
        let (start, end) = device.desired_thresholds();
        device.accepted_reported_start_percent = None;
        device.accepted_reported_end_percent = None;
        self.apply_policy_change(&battery_id, start, end, previous_config, next_config)
            .await
    }

    async fn battery_charge_once(&self, params: Value) -> Response {
        let request: BatteryRequest = decode_request(params, "battery.chargeOnce")?;
        let _guard = battery::lock_effects().await;
        let (battery_id, restore_start, restore_end) = self
            .state
            .read(|s| {
                require_power(&s.battery, "battery.chargeOnce")?;
                let device = requested_device(request.battery_id.as_deref(), &s.battery)?;
                let (start, end) = restoration_thresholds(device, "battery.chargeOnce")?;
                Ok::<_, Value>((device.id.clone(), start, end))
            })
            .await?;
        let previous_runtime = load_runtime().await?;
        if previous_runtime.has_durable_operation() {
            return Err(error(
                ErrorCode::OperationActive,
                "another durable battery operation is already active",
            ));
        }
        let mut runtime = config::BatteryRuntimeState::start_charge_once(
            crate::time::unix_ms(),
            battery_id.clone(),
            restore_start,
            restore_end,
        );
        runtime
            .authorization_uncertain_batteries
            .clone_from(&previous_runtime.authorization_uncertain_batteries);
        save_runtime(&runtime).await?;
        match battery::helper::set_thresholds(&battery_id, 0, 100).await {
            Ok(result) => self.hardware_response(&battery_id, Some(result)).await,
            Err(value) => Err(rollback_runtime(&previous_runtime, value, "charge-once").await),
        }
    }

    async fn battery_set_charging_inhibited(&self, params: Value) -> Response {
        let request: InhibitionRequest = decode_request(params, "battery.setChargingInhibited")?;
        let _guard = battery::lock_effects().await;
        let battery_id = requested_battery_id(request.battery_id.as_deref(), &self.state).await?;
        self.state
            .read(|s| {
                require_behaviour(
                    &s.battery,
                    &battery_id,
                    "inhibit-charge",
                    "inhibit charging",
                )
                .map(|_| ())
            })
            .await?;
        let previous = load_runtime().await?;
        if request.enabled {
            start_charging_inhibition(&battery_id, &previous).await
        } else {
            stop_charging_inhibition(&battery_id, previous).await
        }?;
        self.hardware_response(&battery_id, None).await
    }

    async fn battery_start_calibration(&self, params: Value) -> Response {
        let request: BatteryRequest = decode_request(params, "battery.startCalibration")?;
        let _guard = battery::lock_effects().await;
        let battery_id = requested_battery_id(request.battery_id.as_deref(), &self.state).await?;
        let (restore_start, restore_end) = self
            .state
            .read(|s| {
                require_power(&s.battery, "battery calibration")?;
                let device = require_behaviour(
                    &s.battery,
                    &battery_id,
                    "force-discharge",
                    "force discharge",
                )?;
                restoration_thresholds(device, "battery calibration")
            })
            .await?;
        let previous = load_runtime().await?;
        begin_calibration(&battery_id, restore_start, restore_end, &previous).await?;
        self.hardware_response(&battery_id, None).await
    }

    async fn battery_cancel_calibration(&self, params: Value) -> Response {
        let request: BatteryRequest = decode_request(params, "battery.cancelCalibration")?;
        let _guard = battery::lock_effects().await;
        let battery_id = requested_battery_id(request.battery_id.as_deref(), &self.state).await?;
        let mut runtime = load_runtime().await?;
        if runtime.operation != config::OperationKind::Calibration
            || runtime.operation_battery_id != battery_id
        {
            return Err(error(
                ErrorCode::OperationInactive,
                format!("battery {battery_id} is not being calibrated"),
            ));
        }
        // Persist cancellation before touching hardware. A failed restoration
        // must retry restoration on restart, never resume force-discharge.
        runtime.operation_phase = config::OperationPhase::Restoring;
        save_runtime(&runtime).await?;
        battery::helper::set_charge_behaviour(&battery_id, "auto")
            .await
            .map_err(helper_error)?;
        let (Some(start), Some(end)) = (
            runtime.operation_restore_start_percent,
            runtime.operation_restore_end_percent,
        ) else {
            return Err(error(
                ErrorCode::StateFailed,
                "calibration restoration thresholds are missing",
            ));
        };
        let result = battery::helper::set_thresholds(&battery_id, start, end)
            .await
            .map_err(helper_error)?;
        runtime
            .complete_calibration_restoration(
                result.actual_start_percent,
                result.actual_end_percent,
                result.verified,
            )
            .map_err(|value| error(ErrorCode::OperationFailed, value.to_string()))?;
        save_runtime(&runtime).await?;
        self.hardware_response(&battery_id, None).await
    }

    async fn battery_set_alert_policy(&self, params: Value) -> Response {
        let request: AlertPolicyRequest = decode_request(params, "battery.setAlertPolicy")?;
        let _guard = battery::lock_effects().await;
        let mut next_config = config::load_config()
            .await
            .map_err(|value| error(ErrorCode::ConfigFailed, value.to_string()))?;
        let requested_actions = [
            request
                .warning_profile
                .filter(|action| *action != next_config.warning_profile()),
            request
                .critical_profile
                .filter(|action| *action != next_config.critical_profile()),
        ];
        request
            .apply_to(&mut next_config)
            .map_err(|value| error(ErrorCode::ValidationError, value.to_string()))?;
        // Keep-current is always valid, even if the profile service is offline.
        let unavailable = self
            .state
            .read(|s| {
                requested_actions
                    .into_iter()
                    .flatten()
                    .filter_map(|action| action.profile())
                    .find(|profile| {
                        s.power_profile.available
                            && !s
                                .power_profile
                                .profiles
                                .iter()
                                .any(|item| item.name == *profile)
                    })
            })
            .await;
        if let Some(profile) = unavailable {
            return Err(error(
                ErrorCode::ValidationError,
                format!("power profile is unavailable: {profile}"),
            ));
        }
        self.save_policy_change(next_config).await
    }

    async fn save_policy_change(&self, next_config: config::BatteryConfig) -> Response {
        save_config(&next_config).await?;
        Ok(self.battery_response(None).await)
    }

    async fn apply_policy_change(
        &self,
        battery_id: &str,
        start: u8,
        end: u8,
        previous_config: config::BatteryConfig,
        mut next_config: config::BatteryConfig,
    ) -> Response {
        save_config(&next_config).await?;
        match battery::helper::set_thresholds(battery_id, start, end).await {
            Ok(result) => {
                let device = next_config.device_mut(battery_id);
                device.accepted_reported_start_percent = Some(result.actual_start_percent);
                device.accepted_reported_end_percent = Some(result.actual_end_percent);
                save_config(&next_config).await?;
                self.hardware_response(battery_id, Some(result)).await
            }
            Err(error_value) => {
                config::save_config(&previous_config).await.map_err(|rollback_error| {
                    error(ErrorCode::OperationFailed, format!(
                        "{error_value}; battery configuration rollback also failed: {rollback_error}"
                    ))
                })?;
                Err(helper_error(error_value))
            }
        }
    }

    // Caller still holds the effects lock: another action cannot install a
    // newer uncertainty fence between this confirmation and clearing it.
    async fn hardware_response(
        &self,
        battery_id: &str,
        result: Option<battery::helper::ThresholdWriteResult>,
    ) -> Response {
        let mut runtime = load_runtime().await?;
        runtime
            .authorization_uncertain_batteries
            .retain(|id| id != battery_id);
        save_runtime(&runtime).await?;
        Ok(self
            .battery_response(result.map(|result| (battery_id, result)))
            .await)
    }

    async fn battery_response(
        &self,
        operation: Option<(&str, battery::helper::ThresholdWriteResult)>,
    ) -> Value {
        battery_wire::response(battery::refresh_state(&self.state).await, operation)
    }
}

async fn load_runtime() -> Result<config::BatteryRuntimeState, Value> {
    config::load_runtime()
        .await
        .map_err(|value| error(ErrorCode::StateFailed, value.to_string()))
}

async fn save_runtime(runtime: &config::BatteryRuntimeState) -> Result<(), Value> {
    config::save_runtime(runtime)
        .await
        .map_err(|value| error(ErrorCode::StateFailed, value.to_string()))
}

async fn save_config(config: &config::BatteryConfig) -> Result<(), Value> {
    config::save_config(config)
        .await
        .map_err(|value| error(ErrorCode::ConfigFailed, value.to_string()))
}

fn require_power(battery: &crate::model::BatteryState, action: &str) -> Result<(), Value> {
    battery.plugged.then_some(()).ok_or_else(|| {
        error(
            ErrorCode::NotPlugged,
            format!("{action} requires external power"),
        )
    })
}

fn restoration_thresholds(
    device: &crate::model::BatteryDeviceState,
    action: &str,
) -> Result<(u8, u8), Value> {
    device
        .protection
        .start_percent
        .zip(device.protection.end_percent)
        .ok_or_else(|| {
            error(
                ErrorCode::ProtectionUnsupported,
                format!("{action} requires readable charge thresholds"),
            )
        })
}

fn require_behaviour<'a>(
    battery: &'a crate::model::BatteryState,
    battery_id: &str,
    behaviour: &str,
    action: &str,
) -> Result<&'a crate::model::BatteryDeviceState, Value> {
    let device = requested_device(Some(battery_id), battery)?;
    device
        .protection
        .available_behaviours
        .iter()
        .any(|value| value == behaviour)
        .then_some(device)
        .ok_or_else(|| {
            error(
                ErrorCode::OperationUnsupported,
                format!("battery {battery_id} cannot {action}"),
            )
        })
}

fn helper_error(failure: anyhow::Error) -> Value {
    if battery::authorization::is_unknown(&failure) {
        crate::api::error("outcome-unknown", failure.to_string())
    } else {
        error(ErrorCode::OperationFailed, failure.to_string())
    }
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
            ErrorCode::OperationActive,
            "another durable battery operation is already active",
        ));
    }
    let mut next = config::BatteryRuntimeState::start_calibration(
        crate::time::unix_ms(),
        battery_id.to_string(),
        restore_start,
        restore_end,
    );
    next.authorization_uncertain_batteries
        .clone_from(&previous.authorization_uncertain_batteries);
    save_runtime(&next).await?;
    if let Err(value) = battery::helper::set_thresholds(battery_id, 0, 100).await {
        return Err(rollback_runtime(previous, value, "calibration").await);
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

async fn rollback_runtime(
    previous: &config::BatteryRuntimeState,
    failure: anyhow::Error,
    operation: &str,
) -> Value {
    if battery::authorization::is_unknown(&failure) {
        // The writer persisted a no-replay fence. Preserve restoration data
        // rather than claiming a failed start could not have touched hardware.
        return crate::api::error("outcome-unknown", failure.to_string());
    }
    match config::save_runtime(previous).await {
        Ok(()) => error(ErrorCode::OperationFailed, failure.to_string()),
        Err(rollback_error) => error(
            ErrorCode::OperationFailed,
            format!("{failure}; {operation} state rollback also failed: {rollback_error}"),
        ),
    }
}

async fn rollback_calibration_hardware(
    battery_id: &str,
    restore_start: u8,
    restore_end: u8,
    previous: &config::BatteryRuntimeState,
    failure: anyhow::Error,
) -> Value {
    if battery::authorization::is_unknown(&failure) {
        return crate::api::error("outcome-unknown", failure.to_string());
    }
    let mut message = failure.to_string();
    match battery::helper::set_thresholds(battery_id, restore_start, restore_end).await {
        Ok(result) if !result.verified => message.push_str(&format!(
            "; threshold rollback reported {}–{} instead of {restore_start}–{restore_end}",
            result.actual_start_percent, result.actual_end_percent
        )),
        Ok(_) => {}
        Err(error) => {
            message.push_str(&format!("; threshold rollback also failed: {error}"));
            if battery::authorization::is_unknown(&error) {
                return crate::api::error("outcome-unknown", message);
            }
        }
    }
    if let Err(error) = config::save_runtime(previous).await {
        message.push_str(&format!(
            "; calibration state rollback also failed: {error}"
        ));
    }
    error(ErrorCode::OperationFailed, message)
}

async fn start_charging_inhibition(
    battery_id: &str,
    previous: &config::BatteryRuntimeState,
) -> Result<(), Value> {
    if previous.has_durable_operation() {
        return Err(error(
            ErrorCode::OperationActive,
            "another durable battery operation is already active",
        ));
    }
    let mut next =
        config::BatteryRuntimeState::start_inhibit(crate::time::unix_ms(), battery_id.to_string());
    next.authorization_uncertain_batteries
        .clone_from(&previous.authorization_uncertain_batteries);
    save_runtime(&next).await?;
    if let Err(value) = battery::helper::set_charge_behaviour(battery_id, "inhibit-charge").await {
        return Err(rollback_runtime(previous, value, "inhibition").await);
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
            ErrorCode::OperationInactive,
            format!("charging is not durably inhibited for {battery_id}"),
        ));
    }
    battery::helper::set_charge_behaviour(battery_id, "auto")
        .await
        .map_err(helper_error)?;
    runtime.clear_operation();
    save_runtime(&runtime).await
}

// Caller holds the battery effects lock through persistence and hardware writes.
async fn editable_policy() -> Result<config::BatteryConfig, Value> {
    let config = config::load_config()
        .await
        .map_err(|value| error(ErrorCode::ConfigFailed, value.to_string()))?;
    let runtime = load_runtime().await?;
    if runtime.has_durable_operation() {
        return Err(error(
            ErrorCode::OperationActive,
            "charge policy cannot change during a durable battery operation",
        ));
    }
    Ok(config)
}

fn requested_device<'a>(
    requested: Option<&str>,
    battery: &'a crate::model::BatteryState,
) -> Result<&'a crate::model::BatteryDeviceState, Value> {
    let device = match requested {
        Some(id) => battery.devices.iter().find(|device| device.id == id),
        None => battery.devices.first(),
    };
    device.ok_or_else(|| {
        error(
            ErrorCode::Unavailable,
            requested.map_or_else(
                || "no controllable system battery is available".into(),
                |id| format!("battery {id} is unavailable"),
            ),
        )
    })
}

async fn requested_battery_id(
    requested: Option<&str>,
    state: &StateStore,
) -> Result<String, Value> {
    state
        .read(|s| requested_device(requested, &s.battery).map(|device| device.id.clone()))
        .await
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn live_decoder_rejections_match_contract_without_dispatching_effects() {
        let fixture = crate::protocol::battery_contract_fixture().await;
        let api = super::BatteryApi::new(crate::state::StateStore::default());
        for (method, case) in fixture["cases"].as_object().unwrap() {
            let params = serde_json::Value::Null;
            let reply = api.dispatch(method, params).await;
            assert_eq!(reply, case["rejected"], "{method}");
        }
        assert_eq!(
            api.dispatch(
                "battery.setThresholds",
                serde_json::json!({"battery_id":"BAT0", "start_percent":80, "end_percent":60})
            )
            .await,
            crate::api::error(
                "validation-error",
                "battery start_percent must be lower than end_percent"
            )
        );
    }
    use super::AlertPolicyRequest;
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
}
