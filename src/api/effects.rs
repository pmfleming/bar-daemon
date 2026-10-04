use super::{error, success};
use crate::{
    audio::controller::{AudioCommand, AudioController},
    brightness::BrightnessService,
    hyprland::HyprlandClient,
    media::MediaService,
    power, sleep,
    state::StateStore,
    updates,
};
use serde::Deserialize;
use serde_json::{Value, json};

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
    #[serde(default)]
    offset_seconds: Option<i64>,
    #[serde(default)]
    mode: Option<crate::model::MediaControlMode>,
}

#[derive(Deserialize)]
struct WorkspaceRequest {
    workspace_id: i64,
    #[serde(default)]
    on_current_monitor: bool,
}

pub(super) struct DesktopEffects {
    state: StateStore,
    hyprland: HyprlandClient,
    audio: AudioController,
    brightness: BrightnessService,
    media: MediaService,
}

impl DesktopEffects {
    pub(super) fn new(
        state: StateStore,
        media: MediaService,
        brightness: BrightnessService,
    ) -> Self {
        Self {
            state: state.clone(),
            hyprland: HyprlandClient::default(),
            audio: AudioController::new(state),
            brightness,
            media,
        }
    }

    pub(super) async fn power_sleep_action(&self, action: &str) -> Value {
        match sleep::perform(action).await {
            Ok(state) => {
                self.state.update_power_sleep(state).await;
                success(
                    json!({"power_sleep": self.state.read(|s| json!(&s.power_sleep)).await, "operation": action}),
                )
            }
            Err(value) => error("power-sleep-operation-failed", format!("{value:#}")),
        }
    }
    async fn publish_sleep(&self, state: crate::model::PowerSleepState) -> Value {
        self.state.update_power_sleep(state).await;
        success(json!({"power_sleep": self.state.read(|s| json!(&s.power_sleep)).await}))
    }

    pub(super) async fn set_keep_awake(&self, params: Value) -> Value {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct KeepAwakeRequest {
            enabled: bool,
        }
        let request = request!(params, KeepAwakeRequest, "powerSleep.setKeepAwake");
        match sleep::set_keep_awake(request.enabled).await {
            Ok(state) => self.publish_sleep(state).await,
            Err(value) => error("keep-awake-failed", format!("{value:#}")),
        }
    }

    pub(super) async fn display_layout(&self, method: &str, params: Value) -> Value {
        match crate::display_policy::layout_action(method, params, &self.state).await {
            Ok(state) => success(json!({"display_policy": state})),
            Err(value) => error("display-layout-failed", format!("{value:#}")),
        }
    }

    pub(super) async fn display_focus(&self, method: &str, params: Value) -> Value {
        match crate::display_policy::focus::action(method, params, &self.state).await {
            Ok(state) => success(json!({"display_policy": state})),
            Err(value) => error("display-focus-failed", format!("{value:#}")),
        }
    }

    pub(super) async fn display_policy_set(&self, params: Value) -> Value {
        let policy = request!(
            params,
            crate::display_policy::DisplayPolicy,
            "displayPolicy.set"
        );
        match crate::display_policy::set(policy, &self.state).await {
            Ok(state) => success(json!({"display_policy": state})),
            Err(error) => super::error("display-policy-failed", format!("{error:#}")),
        }
    }

    pub(super) async fn sleep_policy_set(&self, params: Value) -> Value {
        let policy = request!(
            params,
            crate::sleep_policy::SleepPolicy,
            "powerSleep.setPolicy"
        );
        match crate::sleep_policy::set(policy, &self.state).await {
            Ok(state) => success(json!({"sleep_policy": state})),
            Err(value) => error("sleep-policy-failed", format!("{value:#}")),
        }
    }

    pub(super) async fn set_critical_battery(&self, params: Value) -> Value {
        let policy = request!(
            params,
            crate::sleep_policy::critical::Policy,
            "powerSleep.setCriticalPolicy"
        );
        match crate::sleep_policy::critical::set_policy(policy, &self.state).await {
            Ok(state) => success(json!({"sleep_policy": state})),
            Err(error) => super::error("critical-battery-policy-failed", format!("{error:#}")),
        }
    }

    pub(super) async fn cancel_critical_battery(&self) -> Value {
        match crate::sleep_policy::critical::cancel(&self.state).await {
            Ok(_) => {
                success(json!({"sleep_policy": self.state.read(|s| json!(&s.sleep_policy)).await}))
            }
            Err(error) => super::error("critical-battery-cancel-failed", format!("{error:#}")),
        }
    }

    pub(super) async fn idle_sleep(&self, params: Value) -> Value {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct IdleRequest {
            sleep_minutes: u32,
            generation: String,
            episode: u64,
        }
        let request = request!(params, IdleRequest, "powerSleep.idle");
        match crate::sleep_policy::idle_sleep(
            request.sleep_minutes,
            &request.generation,
            request.episode,
            &self.state,
        )
        .await
        {
            Ok(state) => self.publish_sleep(state).await,
            Err(value) => error("automatic-sleep-failed", format!("{value:#}")),
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
        let battery = self.state.read(|s| s.battery.clone()).await;
        self.power_profile_result(power::set_profile(&request.profile, &battery).await)
            .await
    }
    pub(super) async fn power_profile_resume_automatic(&self) -> Value {
        let battery = self.state.read(|s| s.battery.clone()).await;
        self.power_profile_result(power::resume_automatic(&battery).await)
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
        self.apply_brightness(self.brightness.adjust(request.delta_percent))
            .await
    }
    pub(super) async fn brightness_set(&self, params: Value) -> Value {
        let request = request!(params, PercentRequest, "brightness.set");
        self.apply_brightness(self.brightness.set(request.percent))
            .await
    }
    async fn apply_brightness(
        &self,
        operation: impl std::future::Future<Output = anyhow::Result<crate::model::BrightnessState>>,
    ) -> Value {
        match operation.await {
            Ok(state) => success(json!({"brightness": state})),
            Err(value) => error("brightness-operation-failed", format!("{value:#}")),
        }
    }
    pub(super) async fn audio_adjust(&self, params: Value) -> Value {
        let request = request!(params, DeltaRequest, "audio.adjust");
        self.apply_audio(AudioCommand::Adjust(request.delta_percent))
            .await
    }
    pub(super) async fn audio_set_muted(&self, params: Value) -> Value {
        let request = request!(params, MuteRequest, "audio.setMuted");
        self.apply_audio(AudioCommand::SetMuted(request.muted))
            .await
    }
    pub(super) async fn audio_set_input_muted(&self, params: Value) -> Value {
        let request = request!(params, MuteRequest, "audio.setInputMuted");
        self.apply_audio(AudioCommand::SetInputMuted(request.muted))
            .await
    }
    async fn apply_audio(&self, command: AudioCommand) -> Value {
        match self.audio.execute(command).await {
            Ok(state) => success(json!({"audio": state})),
            Err(value) => error("audio-operation-failed", value),
        }
    }
    pub(super) async fn media_operation(&self, params: Value) -> Value {
        let request = request!(params, MediaRequest, "media.operation");
        let policy_result = match request.operation.as_str() {
            "cycle" => Some(self.media.cycle(&self.state).await),
            "automatic" => Some(self.media.select(&self.state, None).await),
            "select" => {
                let Some(id) = request.player_id.as_deref() else {
                    return error("validation-error", "media.select requires player_id");
                };
                Some(self.media.select(&self.state, Some(id)).await)
            }
            "set-mode" => {
                let (Some(id), Some(mode)) = (request.player_id.as_deref(), request.mode) else {
                    return error(
                        "validation-error",
                        "media.set-mode requires player_id and mode",
                    );
                };
                Some(self.media.set_mode(&self.state, id, mode).await)
            }
            _ => None,
        };
        if let Some(result) = policy_result {
            return match result {
                Ok(state) => success(json!({
                    "operation": request.operation,
                    "player_id": state.active_player,
                    "media": state,
                })),
                Err(value) => error("media-operation-failed", value.to_string()),
            };
        }
        let player_id = match request.player_id {
            Some(player_id) => Some(player_id),
            None => self.state.read(|s| s.media.active_player.clone()).await,
        };
        if request.operation == "seek" {
            let Some(offset_seconds) = request.offset_seconds else {
                return error("validation-error", "media.seek requires offset_seconds");
            };
            return match self.media.seek(player_id.as_deref(), offset_seconds).await {
                Ok(player_id) => success(json!({
                    "operation": request.operation,
                    "player_id": player_id,
                    "offset_seconds": offset_seconds,
                })),
                Err(value) => error("media-operation-failed", value.to_string()),
            };
        }
        match self
            .media
            .operation(player_id.as_deref(), &request.operation)
            .await
        {
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
