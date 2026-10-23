//! Audio, brightness and power-profile v1 control contracts. No effect execution here.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{BrightnessState, PowerProfileState};

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct ProfileRequest {
    pub profile: String,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct EnabledRequest {
    pub enabled: bool,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct ProfileActionRequest {
    pub action: String,
    pub enabled: bool,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct DeltaRequest {
    pub delta_percent: i16,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct PercentRequest {
    pub percent: u8,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct MuteRequest {
    #[serde(default)]
    pub muted: Option<bool>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Reply<'a> {
    Brightness {
        brightness: &'a BrightnessState,
    },
    PowerProfile {
        power_profile: &'a PowerProfileState,
    },
}

/// Reuse the exact live decoder/result DTOs; examples do not define schemas.
pub(super) fn contract<P: schemars::JsonSchema, R: schemars::JsonSchema>(
    name: &str,
    key: &str,
    params: Value,
) -> Value {
    let mut entry = shelllist_daemon_core::method_schemas::<P, R>();
    entry["name"] = name.into();
    entry["response_key"] = key.into();
    entry["params_example"] = params;
    entry["policy"] = serde_json::to_value(policy(key)).expect("method policy");
    entry
}

pub(crate) fn policy(key: &str) -> shelllist_daemon_core::MethodPolicy {
    use shelllist_daemon_core::{ExecutionMode, MethodPolicy, Sensitivity};
    MethodPolicy::new(
        ExecutionMode::Effect,
        Sensitivity::Effect,
        if key == "brightness" {
            "brightness"
        } else {
            "power-profile"
        },
    )
}

pub(crate) fn brightness(result: anyhow::Result<BrightnessState>) -> Value {
    match result {
        Ok(state) => super::serialized_success(Reply::Brightness { brightness: &state }),
        // Brightness includes the error chain; power-profile exposes only the top-level message.
        Err(error) => super::error("brightness-operation-failed", format!("{error:#}")),
    }
}
pub(crate) fn power_profile(result: &anyhow::Result<PowerProfileState>) -> Value {
    match result {
        Ok(state) => super::serialized_success(Reply::PowerProfile {
            power_profile: state,
        }),
        Err(error) => super::error("power-profile-operation-failed", error.to_string()),
    }
}
