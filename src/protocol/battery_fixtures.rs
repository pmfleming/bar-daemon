//! Pure policy/runtime transitions and production serializers; never battery helper calls.
use super::{
    battery_wire::{
        self, AlertPolicyRequest, BatteryRequest, ErrorCode, InhibitionRequest, ProtectionRequest,
        ThresholdRequest, error,
    },
    read_wire,
};
use crate::{
    api,
    battery::{
        self,
        config::{BatteryConfig, BatteryRuntimeState, OperationPhase},
        helper::ThresholdWriteResult,
    },
    model::{BatteryProfileAction, BatteryState},
    state::StateStore,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

fn params<T: Serialize + serde::de::DeserializeOwned>(request: T, method: &str) -> (Value, Value) {
    (
        serde_json::to_value(request).unwrap(),
        api::decode_request::<T>(Value::Null, method).err().unwrap(),
    )
}
fn observed(
    mut base: BatteryState,
    config: &BatteryConfig,
    runtime: &BatteryRuntimeState,
    start: u8,
    end: u8,
) -> BatteryState {
    let device = base
        .devices
        .iter_mut()
        .find(|device| device.id == "BAT0")
        .unwrap();
    battery::update_observed_thresholds(device, start, end);
    let behaviour = match (runtime.operation, runtime.operation_phase) {
        (battery::config::OperationKind::Inhibit, _) => "inhibit-charge",
        (battery::config::OperationKind::Calibration, OperationPhase::Discharging) => {
            "force-discharge"
        }
        _ => "auto",
    };
    device.protection.charge_behaviour = Some(behaviour.into());
    battery::decorate(base, config, runtime, None)
}
pub(super) async fn fixture() -> Value {
    let mut base: BatteryState =
        serde_json::from_value(super::contract_fixture().unwrap()["snapshot"]["battery"].clone())
            .unwrap();
    base.plugged = true;
    let mut config = BatteryConfig::default();
    config.device_mut("BAT0").manage_thresholds = true;
    config.device_mut("BAT0").protection_enabled = true;
    let idle = BatteryRuntimeState::default();
    base = observed(base, &config, &idle, 75, 80);
    let store = StateStore::default();
    store.update_battery(base.clone()).await;
    let snapshot = read_wire::snapshot(&store.snapshot().await);
    let mut cases = Map::new();
    for method in [
        "battery.setThresholds",
        "battery.setProtection",
        "battery.chargeOnce",
        "battery.setChargingInhibited",
        "battery.startCalibration",
        "battery.cancelCalibration",
        "battery.setAlertPolicy",
    ] {
        let mut policy = config.clone();
        let mut runtime = idle.clone();
        let mut actual = (75, 80);
        let mut has_readback = false;
        let (params, rejected) = match method {
            "battery.setThresholds" | "battery.setProtection" => {
                policy.device_mut("BAT0").protected_start_percent = 60;
                actual = (60, 80);
                has_readback = true;
                if method.ends_with("setThresholds") {
                    params(
                        ThresholdRequest {
                            battery_id: "BAT0".into(),
                            start_percent: 60,
                            end_percent: 80,
                        },
                        method,
                    )
                } else {
                    params(
                        ProtectionRequest {
                            battery_id: Some("BAT0".into()),
                            enabled: true,
                            start_percent: Some(60),
                            end_percent: Some(80),
                        },
                        method,
                    )
                }
            }
            "battery.chargeOnce" => {
                runtime = BatteryRuntimeState::start_charge_once(
                    1_700_000_000_000,
                    "BAT0".into(),
                    75,
                    80,
                );
                actual = (0, 100);
                has_readback = true;
                params(
                    BatteryRequest {
                        battery_id: Some("BAT0".into()),
                    },
                    method,
                )
            }
            "battery.setChargingInhibited" => {
                runtime = BatteryRuntimeState::start_inhibit(1_700_000_000_000, "BAT0".into());
                params(
                    InhibitionRequest {
                        battery_id: Some("BAT0".into()),
                        enabled: true,
                    },
                    method,
                )
            }
            "battery.startCalibration" => {
                runtime = BatteryRuntimeState::start_calibration(
                    1_700_000_000_000,
                    "BAT0".into(),
                    75,
                    80,
                );
                actual = (0, 100);
                params(
                    BatteryRequest {
                        battery_id: Some("BAT0".into()),
                    },
                    method,
                )
            }
            "battery.cancelCalibration" => {
                runtime = BatteryRuntimeState::start_calibration(
                    1_700_000_000_000,
                    "BAT0".into(),
                    75,
                    80,
                );
                runtime.operation_phase = OperationPhase::Restoring;
                runtime
                    .complete_calibration_restoration(75, 80, true)
                    .unwrap();
                params(
                    BatteryRequest {
                        battery_id: Some("BAT0".into()),
                    },
                    method,
                )
            }
            _ => {
                let request = AlertPolicyRequest {
                    warning_percent: Some(30),
                    critical_percent: Some(10),
                    warning_profile: Some(BatteryProfileAction::KeepCurrent),
                    ..Default::default()
                };
                let result = params(request, method);
                let decoded: AlertPolicyRequest = serde_json::from_value(result.0.clone()).unwrap();
                decoded.apply_to(&mut policy).unwrap();
                result
            }
        };
        let readback = |start, end, verified| {
            has_readback.then_some((
                "BAT0",
                ThresholdWriteResult {
                    actual_start_percent: start,
                    actual_end_percent: end,
                    verified,
                },
            ))
        };
        let success = battery_wire::response(
            Ok(observed(
                base.clone(),
                &policy,
                &runtime,
                actual.0,
                actual.1,
            )),
            readback(actual.0, actual.1, true),
        );
        let limited = has_readback.then(|| {
            battery_wire::response(
                Ok(observed(base.clone(), &policy, &runtime, 0, 80)),
                readback(0, 80, false),
            )
        });
        cases.insert(method.into(), json!({"params":params, "success":success, "limited":limited, "rejected":rejected,
            "failed":if method == "battery.setAlertPolicy" {
                    error(ErrorCode::ConfigFailed, "Battery settings could not be saved")
                } else {
                    error(ErrorCode::OperationFailed, "Battery helper rejected the operation")
                },
            "readbackFailed":battery_wire::response(Err(anyhow::anyhow!("Battery readback unavailable; operation may have completed")), None)}));
    }
    let mut runtime =
        BatteryRuntimeState::start_calibration(1_700_000_000_000, "BAT0".into(), 75, 80);
    let mut events = Map::new();
    for (name, phase) in [
        ("discharging", OperationPhase::Discharging),
        ("charging", OperationPhase::Charging),
        ("restoring", OperationPhase::Restoring),
    ] {
        runtime.operation_phase = phase;
        let state = observed(base.clone(), &config, &runtime, 0, 100);
        let (_, mut receiver) = store.snapshot_and_subscribe().await;
        store.update_battery(state).await;
        let event = receiver.recv().await.unwrap();
        events.insert(
            name.into(),
            read_wire::Event::changed("subscription-example", event).into_value(),
        );
    }
    let failed = runtime
        .complete_calibration_restoration(0, 100, false)
        .unwrap_err();
    let restoration_failed = error(ErrorCode::OperationFailed, failed.to_string());
    runtime
        .complete_calibration_restoration(75, 80, true)
        .unwrap();
    let (_, mut receiver) = store.snapshot_and_subscribe().await;
    store
        .update_battery(observed(base.clone(), &config, &runtime, 75, 80))
        .await;
    events.insert(
        "completed".into(),
        read_wire::Event::changed("subscription-example", receiver.recv().await.unwrap())
            .into_value(),
    );
    json!({"snapshot":snapshot, "cases":cases, "history":battery_wire::history(base.history), "events":events, "restorationFailed":restoration_failed,
        "operationStarted": crate::daemon::battery_operations::started("battery-operation-example"),
        "operationCancelled": crate::daemon::battery_operations::status("battery-operation-example", None, true),
        "operationRestorationFailed": crate::daemon::battery_operations::status("battery-operation-example", Some(restoration_failed.clone()), false),
        "operationFinished": cases.iter().map(|(method, case)| (method.clone(), Value::Object(case.as_object().unwrap().iter().filter(|(_, response)| response.get("ok").is_some()).map(|(outcome, response)| (outcome.clone(), crate::daemon::battery_operations::status("battery-operation-example", Some(response.clone()), false))).collect()))).collect::<Map<_, _>>(),
        "operationUnknown": crate::daemon::battery_operations::status("battery-operation-example", Some(crate::api::error("outcome-unknown", "Battery write outcome unknown; reconcile state")), false)
    })
}
