//! Offline control examples use live DTOs, reply constructors and request validation.
//! Hardware commands are never run by this generator.
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

use super::control_wire::*;
use crate::{
    api,
    model::{AudioState, BrightnessState, PowerProfile, PowerProfileAction, PowerProfileState},
};

#[derive(Serialize)]
struct Case {
    params: Value,
    success: Value,
    limited: Value,
    failed: Value,
    invalid_params: Option<Value>,
    rejected: Option<Value>,
}

fn rejection<T: DeserializeOwned>(method: &str, params: Value) -> (Option<Value>, Option<Value>) {
    let reply = api::decode_request::<T>(params.clone(), method)
        .err()
        .expect("invalid example must be rejected");
    (Some(params), Some(reply))
}

fn case(
    params: impl Serialize,
    success: Value,
    limited: Value,
    failed: Value,
    (invalid_params, rejected): (Option<Value>, Option<Value>),
) -> Value {
    serde_json::to_value(Case {
        params: serde_json::to_value(params).unwrap(),
        success,
        limited,
        failed,
        invalid_params,
        rejected,
    })
    .expect("serializable control fixture")
}

pub(super) fn fixture() -> Value {
    let mut fixture = Map::new();
    for method in ["audio.adjust", "audio.setMuted", "audio.setInputMuted"] {
        let mut state = AudioState {
            available: true,
            volume_percent: 65,
            sink_name: "alsa_output.test".into(),
            sink_description: "Speakers".into(),
            input_available: true,
            source_name: "alsa_input.test".into(),
            source_description: "Microphone".into(),
            ..Default::default()
        };
        state.muted = method == "audio.setMuted";
        state.input_muted = method == "audio.setInputMuted";
        let mut limited = state.clone();
        limited.volume_percent = 100; // An adjustment can reach the upper bound.
        let (params, rejected) = if method == "audio.adjust" {
            (
                serde_json::to_value(DeltaRequest { delta_percent: 15 }).unwrap(),
                rejection::<DeltaRequest>(method, json!({"delta_percent": "loud"})),
            )
        } else {
            (
                serde_json::to_value(MuteRequest { muted: None }).unwrap(),
                rejection::<MuteRequest>(method, json!({"muted": "true"})),
            )
        };
        fixture.insert(
            method.into(),
            case(
                params,
                audio(Ok(state)),
                audio(Ok(limited)),
                audio(Err("PipeWire disconnected before readback".into())),
                rejected,
            ),
        );
    }
    for method in ["brightness.adjust", "brightness.set"] {
        let state = BrightnessState {
            available: true,
            device: "amdgpu_bl1".into(),
            brightness: 650,
            max_brightness: 1000,
            percent: 65,
            error: None,
        };
        let mut limited = state.clone();
        // Coarse hardware granularity can read back a different percentage.
        limited.brightness = 2;
        limited.max_brightness = 3;
        limited.percent = 67;
        let (params, rejected) = if method == "brightness.adjust" {
            (
                serde_json::to_value(DeltaRequest { delta_percent: 15 }).unwrap(),
                rejection::<DeltaRequest>(method, json!({"delta_percent": "bright"})),
            )
        } else {
            (
                serde_json::to_value(PercentRequest { percent: 65 }).unwrap(),
                rejection::<PercentRequest>(method, json!({"percent": "bright"})),
            )
        };
        fixture.insert(
            method.into(),
            case(
                params,
                brightness(Ok(state)),
                brightness(Ok(limited)),
                brightness(Err(
                    anyhow::anyhow!("device disappeared").context("backlight readback failed")
                )),
                rejected,
            ),
        );
    }
    for method in [
        "powerProfile.set",
        "powerProfile.resumeAutomatic",
        "powerProfile.setBatteryAware",
        "powerProfile.setActionEnabled",
    ] {
        let mut state = PowerProfileState {
            available: true,
            profile: "balanced".into(),
            battery_aware: Some(false),
            profiles: ["power-saver", "balanced", "performance"]
                .map(|name| PowerProfile {
                    name: name.into(),
                    ..Default::default()
                })
                .to_vec(),
            actions: vec![PowerProfileAction {
                name: "amdgpu_panel_power".into(),
                enabled: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        state.battery_automation.level = "normal".into();
        state.battery_automation.status = "waiting".into();
        let (params, rejected) = match method {
            "powerProfile.set" => {
                state.profile = "performance".into();
                state.battery_automation.level = "warning".into();
                state.battery_automation.profile = "power-saver".into();
                state.battery_automation.status = "paused".into();
                (
                    serde_json::to_value(ProfileRequest {
                        profile: "performance".into(),
                    })
                    .unwrap(),
                    rejection::<ProfileRequest>(method, json!({"profile": false})),
                )
            }
            "powerProfile.setBatteryAware" => {
                state.battery_aware = Some(true);
                (
                    serde_json::to_value(EnabledRequest { enabled: true }).unwrap(),
                    rejection::<EnabledRequest>(method, json!({"enabled": "true"})),
                )
            }
            "powerProfile.setActionEnabled" => {
                state.actions[0].enabled = true;
                (
                    serde_json::to_value(ProfileActionRequest {
                        action: "amdgpu_panel_power".into(),
                        enabled: true,
                    })
                    .unwrap(),
                    rejection::<ProfileActionRequest>(
                        method,
                        json!({"action": "amdgpu_panel_power", "enabled": "true"}),
                    ),
                )
            }
            "powerProfile.resumeAutomatic" => {
                state.profile = "power-saver".into();
                state.battery_automation.level = "warning".into();
                state.battery_automation.profile = "power-saver".into();
                state.battery_automation.status = "active".into();
                (json!({}), (None, None)) // This method has no request decoder in v1.
            }
            _ => unreachable!("unregistered power-profile fixture method"),
        };
        let mut limited = state.clone();
        limited.performance_degraded = "high-operating-temperature".into();
        limited.battery_aware = None; // Optional telemetry may be unavailable after a successful effect.
        fixture.insert(
            method.into(),
            case(
                params,
                power_profile(&Ok(state)),
                power_profile(&Ok(limited)),
                power_profile(&Err(
                    anyhow::anyhow!("denied").context("Power Profiles operation rejected")
                )),
                rejected,
            ),
        );
    }
    Value::Object(fixture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_control_examples_are_registered_and_limitations_remain_successful_observations() {
        let fixtures = fixture();
        assert_eq!(fixtures.as_object().unwrap().len(), 9);
        for (method, case) in fixtures.as_object().unwrap() {
            assert!(super::super::METHODS.contains(&method.as_str()));
            let field = if method.starts_with("audio.") {
                "audio"
            } else if method.starts_with("brightness.") {
                "brightness"
            } else {
                "power_profile"
            };
            for outcome in ["success", "limited"] {
                assert_eq!(case[outcome]["ok"], true, "{method}/{outcome}");
                assert!(case[outcome]["data"][field].is_object());
                assert!(case[outcome].get("error").is_none());
            }
            assert_eq!(case["failed"]["ok"], false);
            if !case["rejected"].is_null() {
                assert_eq!(case["rejected"]["error"]["code"], "validation-error");
            }
        }
    }
}
