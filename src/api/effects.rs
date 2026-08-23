use std::sync::Arc;

use super::{error, success};
use crate::{
    audio, brightness, hyprland::HyprlandClient, media, power, sleep, state::StateStore, updates,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Deserialize)]
struct ProfileRequest {
    profile: String,
}

#[derive(Deserialize)]
struct EnabledRequest {
    enabled: bool,
}

#[derive(Deserialize)]
struct ProfileActionRequest {
    action: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct DeltaRequest {
    delta_percent: i16,
}

#[derive(Deserialize)]
struct PercentRequest {
    percent: u8,
}

#[derive(Deserialize)]
struct MuteRequest {
    #[serde(default)]
    muted: Option<bool>,
}

#[derive(Deserialize)]
struct MediaRequest {
    operation: String,
    #[serde(default)]
    player_id: Option<String>,
}

#[derive(Deserialize)]
struct WorkspaceRequest {
    workspace_id: i64,
    #[serde(default)]
    on_current_monitor: bool,
}

#[derive(Clone)]
pub(super) struct DesktopEffects {
    state: StateStore,
    hyprland: Arc<HyprlandClient>,
    audio: Arc<Mutex<()>>,
    brightness: Arc<Mutex<()>>,
}

impl DesktopEffects {
    pub(super) fn new(state: StateStore) -> Self {
        Self {
            state,
            hyprland: Arc::new(HyprlandClient::default()),
            audio: Arc::new(Mutex::new(())),
            brightness: Arc::new(Mutex::new(())),
        }
    }

    pub(super) async fn power_sleep_action(&self, action: &str) -> Value {
        match sleep::perform(action).await {
            Ok(state) => {
                let response = success(json!({"power_sleep": state, "operation": action}));
                self.state.update_power_sleep(state).await;
                response
            }
            Err(value) => error("power-sleep-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn updates_refresh(&self) -> Value {
        match updates::refresh_default(&self.state).await {
            Ok(state) => success(json!({"updates": state})),
            Err(value) => error("update-refresh-failed", value.to_string()),
        }
    }
    pub(super) async fn power_profile_set(&self, params: Value) -> Value {
        let request = request!(params, ProfileRequest, "powerProfile.set");
        let battery = self.state.snapshot().await.battery;
        self.power_profile_result(power::set_profile(&request.profile, &battery).await)
            .await
    }
    pub(super) async fn power_profile_set_battery_aware(&self, params: Value) -> Value {
        let request = request!(params, EnabledRequest, "powerProfile.setBatteryAware");
        self.power_profile_result(power::set_battery_aware(request.enabled).await)
            .await
    }
    pub(super) async fn power_profile_set_action_enabled(&self, params: Value) -> Value {
        let request = request!(
            params,
            ProfileActionRequest,
            "powerProfile.setActionEnabled"
        );
        self.power_profile_result(power::set_action_enabled(&request.action, request.enabled).await)
            .await
    }
    async fn power_profile_result(
        &self,
        value: anyhow::Result<crate::model::PowerProfileState>,
    ) -> Value {
        match value {
            Ok(state) => {
                let response = success(json!({"power_profile": state}));
                self.state.update_power_profile(state).await;
                response
            }
            Err(value) => error("power-profile-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn brightness_adjust(&self, params: Value) -> Value {
        let request = request!(params, DeltaRequest, "brightness.adjust");
        self.apply_brightness(brightness::adjust(request.delta_percent))
            .await
    }
    pub(super) async fn brightness_set(&self, params: Value) -> Value {
        let request = request!(params, PercentRequest, "brightness.set");
        self.apply_brightness(brightness::set(request.percent))
            .await
    }
    async fn apply_brightness(
        &self,
        operation: impl std::future::Future<Output = anyhow::Result<crate::model::BrightnessState>>,
    ) -> Value {
        let _guard = self.brightness.lock().await;
        match operation.await {
            Ok(state) => {
                let response = success(json!({"brightness": state}));
                self.state.update_brightness(state).await;
                response
            }
            Err(value) => error("brightness-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn audio_adjust(&self, params: Value) -> Value {
        let request = request!(params, DeltaRequest, "audio.adjust");
        self.apply_audio(audio::adjust(request.delta_percent)).await
    }
    pub(super) async fn audio_set_muted(&self, params: Value) -> Value {
        let request = request!(params, MuteRequest, "audio.setMuted");
        self.apply_audio(audio::set_muted(request.muted)).await
    }
    pub(super) async fn audio_set_input_muted(&self, params: Value) -> Value {
        let request = request!(params, MuteRequest, "audio.setInputMuted");
        self.apply_audio(audio::set_input_muted(request.muted))
            .await
    }
    async fn apply_audio(
        &self,
        operation: impl std::future::Future<Output = anyhow::Result<crate::model::AudioState>>,
    ) -> Value {
        let _guard = self.audio.lock().await;
        match operation.await {
            Ok(state) => {
                let response = success(json!({"audio": state}));
                self.state.update_audio(state).await;
                response
            }
            Err(value) => error("audio-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn media_operation(&self, params: Value) -> Value {
        let request = request!(params, MediaRequest, "media.operation");
        match media::operation(request.player_id.as_deref(), &request.operation).await {
            Ok(player_id) => {
                success(json!({"operation": request.operation, "player_id": player_id}))
            }
            Err(value) => error("media-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn focus_workspace(&self, params: Value) -> Value {
        let request = request!(params, WorkspaceRequest, "workspace.focus");
        match self
            .hyprland
            .focus_workspace(request.workspace_id, request.on_current_monitor)
            .await
        {
            Ok(()) => success(json!({
                "operation": "workspace.focus",
                "workspace_id": request.workspace_id,
                "on_current_monitor": request.on_current_monitor
            })),
            Err(value) => error("workspace-focus-failed", value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{model::PowerProfileState, state::StateStore};

    use super::DesktopEffects;

    #[tokio::test]
    async fn successful_power_profile_effect_updates_the_snapshot() {
        let store = StateStore::default();
        let effects = DesktopEffects::new(store.clone());
        let state = PowerProfileState {
            available: true,
            profile: "balanced".into(),
            ..PowerProfileState::default()
        };

        let response = effects.power_profile_result(Ok(state.clone())).await;

        assert_eq!(response["data"]["power_profile"]["profile"], "balanced");
        assert_eq!(store.snapshot().await.power_profile, state);
    }
}
