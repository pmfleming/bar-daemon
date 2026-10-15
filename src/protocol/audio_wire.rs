//! Audio declarations own wire metadata and typed dispatch, not desktop effects.
use std::future::Future;

use super::{
    control_wire::{DeltaRequest, MuteRequest},
    stream,
};
use crate::model::AudioState;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use shelllist_daemon_core::{ExecutionMode, MethodPolicy, Sensitivity};

/// Transport implementations supply effects; the contract never imports an API service.
pub(crate) trait AudioHandler: Sync {
    fn audio_adjust(
        &self,
        params: DeltaRequest,
        owner: Option<String>,
    ) -> impl Future<Output = Result<AudioState, Value>> + Send;
    fn audio_set_muted(
        &self,
        params: MuteRequest,
        owner: Option<String>,
    ) -> impl Future<Output = Result<AudioState, Value>> + Send;
    fn audio_set_input_muted(
        &self,
        params: MuteRequest,
        owner: Option<String>,
    ) -> impl Future<Output = Result<AudioState, Value>> + Send;
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Reply {
    Audio { audio: AudioState },
}
fn decode<T: DeserializeOwned>(params: Value) -> Result<T, Value> {
    serde_json::from_value(params)
        .map_err(|error| super::error("validation-error", error.to_string()))
}
const POLICY: MethodPolicy = MethodPolicy::new(ExecutionMode::Effect, Sensitivity::Effect, "audio");
shelllist_daemon_core::declare_methods! {
    api: impl AudioHandler, error: Value, reply: Reply,
    decode: decode, unsupported: super::unsupported_method;
    Adjust {
        name: "audio.adjust", params: DeltaRequest, result: AudioState, handler: audio_adjust,
        reply: |audio| Reply::Audio { audio }, response_key: "audio",
        policy: POLICY, stream: stream::AUDIO, example: DeltaRequest { delta_percent: 5 },
    }
    SetMuted {
        name: "audio.setMuted", params: MuteRequest, result: AudioState, handler: audio_set_muted,
        reply: |audio| Reply::Audio { audio }, response_key: "audio",
        policy: POLICY, stream: stream::AUDIO, example: MuteRequest { muted: None },
    }
    SetInputMuted {
        name: "audio.setInputMuted", params: MuteRequest, result: AudioState, handler: audio_set_input_muted,
        reply: |audio| Reply::Audio { audio }, response_key: "audio",
        policy: POLICY, stream: stream::AUDIO, example: MuteRequest { muted: None },
    }
}
