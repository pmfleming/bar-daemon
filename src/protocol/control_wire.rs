//! Audio, brightness and power-profile v1 control contracts. No effect execution here.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api,
    model::{AudioState, BrightnessState, PowerProfileState},
};

#[derive(Deserialize, Serialize)]
pub(crate) struct ProfileRequest {
    pub profile: String,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct EnabledRequest {
    pub enabled: bool,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct ProfileActionRequest {
    pub action: String,
    pub enabled: bool,
}
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct DeltaRequest {
    pub delta_percent: i16,
}
#[derive(Deserialize, Serialize)]
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
