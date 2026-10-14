//! Audio, brightness and power-profile v1 control contracts. No effect execution here.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api,
    model::{AudioState, BrightnessState, PowerProfileState},
};

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
    Audio {
        audio: &'a AudioState,
    },
    Brightness {
        brightness: &'a BrightnessState,
    },
    PowerProfile {
        power_profile: &'a PowerProfileState,
    },
}
impl Reply<'_> {
    fn envelope(self) -> Value {
        api::success(serde_json::to_value(self).expect("serializable control reply"))
    }
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

enum Failure<'a> {
    Audio(&'a str),
    Brightness(&'a anyhow::Error),
    PowerProfile(&'a anyhow::Error),
}
impl Failure<'_> {
    fn envelope(self) -> Value {
        match self {
            Self::Audio(message) => api::error("audio-operation-failed", message),
            // Preserve existing formatting: brightness includes the error chain,
            // while power-profile replies expose only the top-level message.
            Self::Brightness(error) => {
                api::error("brightness-operation-failed", format!("{error:#}"))
            }
            Self::PowerProfile(error) => {
                api::error("power-profile-operation-failed", error.to_string())
            }
        }
    }
}

pub(crate) fn audio(result: Result<AudioState, String>) -> Value {
    match result {
        Ok(state) => Reply::Audio { audio: &state }.envelope(),
        Err(error) => Failure::Audio(&error).envelope(),
    }
}
pub(crate) fn brightness(result: anyhow::Result<BrightnessState>) -> Value {
    match result {
        Ok(state) => Reply::Brightness { brightness: &state }.envelope(),
        Err(error) => Failure::Brightness(&error).envelope(),
    }
}
pub(crate) fn power_profile(result: &anyhow::Result<PowerProfileState>) -> Value {
    match result {
        Ok(state) => Reply::PowerProfile {
            power_profile: state,
        }
        .envelope(),
        Err(error) => Failure::PowerProfile(error).envelope(),
    }
}
