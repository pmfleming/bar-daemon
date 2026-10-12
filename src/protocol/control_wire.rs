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
#[derive(Deserialize, Serialize)]
pub(crate) struct DeltaRequest {
    pub delta_percent: i16,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct PercentRequest {
    pub percent: u8,
}
#[derive(Deserialize, Serialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_v1_keys_omissions_and_error_formatting() {
        let audio_state = AudioState::default();
        assert_eq!(
            audio(Ok(audio_state.clone())),
            api::success(json!({"audio": audio_state}))
        );
        let brightness_state = BrightnessState::default();
        assert_eq!(
            brightness(Ok(brightness_state.clone())),
            api::success(json!({"brightness": brightness_state}))
        );
        let profile = PowerProfileState::default();
        assert_eq!(
            power_profile(&Ok(profile.clone())),
            api::success(json!({"power_profile": profile}))
        );
        let cause = || anyhow::anyhow!("cause").context("context");
        for (reply, code, message) in [
            (
                audio(Err("context: cause".into())),
                "audio-operation-failed",
                "context: cause",
            ),
            (
                brightness(Err(cause())),
                "brightness-operation-failed",
                "context: cause",
            ),
            (
                power_profile(&Err(cause())),
                "power-profile-operation-failed",
                "context",
            ),
        ] {
            assert_eq!(reply, api::error(code, message));
            assert!(reply.get("data").is_none());
            assert!(reply["error"].get("retryable").is_none());
        }
    }
}
