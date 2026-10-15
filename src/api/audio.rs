//! Desktop effects implementing the transport-independent audio contract.
use super::ApiService;
use crate::{
    audio::controller::AudioCommand,
    model::AudioState,
    protocol::{
        audio_wire::{AudioHandler, Request},
        control_wire::{DeltaRequest, MuteRequest},
    },
};
use serde_json::Value;

pub(super) async fn dispatch(api: &ApiService, method: &str, params: Value) -> Value {
    let result = match Request::decode(method, params) {
        Ok(request) => request.dispatch(api, None).await,
        Err(error) => return error,
    };
    result.map_or_else(
        |error| error,
        |reply| super::success(serde_json::to_value(reply).expect("audio reply")),
    )
}

impl AudioHandler for ApiService {
    async fn audio_adjust(
        &self,
        params: DeltaRequest,
        _owner: Option<String>,
    ) -> Result<AudioState, Value> {
        self.effects
            .apply_audio(AudioCommand::Adjust(params.delta_percent))
            .await
    }
    async fn audio_set_muted(
        &self,
        params: MuteRequest,
        _owner: Option<String>,
    ) -> Result<AudioState, Value> {
        self.effects
            .apply_audio(AudioCommand::SetMuted(params.muted))
            .await
    }
    async fn audio_set_input_muted(
        &self,
        params: MuteRequest,
        _owner: Option<String>,
    ) -> Result<AudioState, Value> {
        self.effects
            .apply_audio(AudioCommand::SetInputMuted(params.muted))
            .await
    }
}
