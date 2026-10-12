use crate::{
    audio::controller::{AudioCommand, AudioController},
    brightness::BrightnessService,
    hyprland::HyprlandClient,
    media::MediaService,
    power,
    protocol::control_wire::{
        self, DeltaRequest, EnabledRequest, MuteRequest, PercentRequest, ProfileActionRequest,
        ProfileRequest,
    },
    protocol::display_wire::{self, Failure as DisplayFailure},
    protocol::session_controls::{self, ErrorCode, MediaRequest, WorkspaceRequest},
    protocol::sleep_wire::{self, Action, Failure, IdleRequest, KeepAwakeRequest},
    sleep,
    state::StateStore,
    updates,
};
use serde_json::Value;

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

    pub(super) async fn power_sleep_action(&self, action: Action) -> Value {
        match sleep::perform(action.as_str()).await {
            Ok(state) => sleep_wire::publish(&self.state, state, Some(action)).await,
            Err(value) => Failure::Operation.reply(&value),
        }
    }
    async fn publish_sleep(&self, state: crate::model::PowerSleepState) -> Value {
        sleep_wire::publish(&self.state, state, None).await
    }

    pub(super) async fn set_keep_awake(&self, params: Value) -> Value {
        let request = request!(params, KeepAwakeRequest, "powerSleep.setKeepAwake");
        match sleep::set_keep_awake(request.enabled).await {
            Ok(state) => self.publish_sleep(state).await,
            Err(value) => Failure::KeepAwake.reply(&value),
        }
    }

    pub(super) async fn display_layout(&self, method: &str, params: Value) -> Value {
        let result = crate::display_policy::layout_action(method, params, &self.state).await;
        display_wire::reply(result, DisplayFailure::Layout)
    }

    pub(super) async fn display_focus(&self, method: &str, params: Value) -> Value {
        let result = crate::display_policy::focus::action(method, params, &self.state).await;
        display_wire::reply(result, DisplayFailure::Focus)
    }

    pub(super) async fn display_policy_set(&self, params: Value) -> Value {
        let policy = request!(
            params,
            crate::display_policy::DisplayPolicy,
            "displayPolicy.set"
        );
        let result = crate::display_policy::set(policy, &self.state).await;
        display_wire::reply(result, DisplayFailure::Policy)
    }

    pub(super) async fn sleep_policy_set(&self, params: Value) -> Value {
        let policy = request!(
            params,
            crate::sleep_policy::SleepPolicy,
            "powerSleep.setPolicy"
        );
        let result = crate::sleep_policy::set(policy, &self.state).await;
        sleep_wire::policy_result(result, Failure::Policy)
    }

    pub(super) async fn set_critical_battery(&self, params: Value) -> Value {
        let policy = request!(
            params,
            crate::sleep_policy::critical::Policy,
            "powerSleep.setCriticalPolicy"
        );
        let result = crate::sleep_policy::critical::set_policy(policy, &self.state).await;
        sleep_wire::policy_result(result, Failure::CriticalPolicy)
    }

    pub(super) async fn cancel_critical_battery(&self) -> Value {
        match crate::sleep_policy::critical::cancel(&self.state).await {
            Ok(_) => {
                self.state
                    .read(|s| sleep_wire::policy(&s.sleep_policy))
                    .await
            }
            Err(error) => Failure::CriticalCancel.reply(&error),
        }
    }

    pub(super) async fn idle_sleep(&self, params: Value) -> Value {
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
            Err(value) => Failure::Automatic.reply(&value),
        }
    }

    pub(super) async fn updates_refresh(&self) -> Value {
        session_controls::updates(updates::refresh_default(&self.state).await)
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
        let response = control_wire::power_profile(&value);
        if let Ok(state) = value {
            self.state.update_power_profile(state).await;
        }
        response
    }
    pub(super) async fn brightness_adjust(&self, params: Value) -> Value {
        let request = request!(params, DeltaRequest, "brightness.adjust");
        control_wire::brightness(self.brightness.adjust(request.delta_percent).await)
    }
    pub(super) async fn brightness_set(&self, params: Value) -> Value {
        let request = request!(params, PercentRequest, "brightness.set");
        control_wire::brightness(self.brightness.set(request.percent).await)
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
        control_wire::audio(self.audio.execute(command).await)
    }
    pub(super) async fn media_operation(&self, params: Value) -> Value {
        let request = request!(params, MediaRequest, "media.operation");
        let policy_result = match request.operation.as_str() {
            "cycle" => Some(self.media.cycle(&self.state).await),
            "automatic" => Some(self.media.select(&self.state, None).await),
            "select" => {
                let Some(id) = request.player_id.as_deref() else {
                    return ErrorCode::Validation.reply("media.select requires player_id");
                };
                Some(self.media.select(&self.state, Some(id)).await)
            }
            "set-mode" => {
                let (Some(id), Some(mode)) = (request.player_id.as_deref(), request.mode) else {
                    return ErrorCode::Validation
                        .reply("media.set-mode requires player_id and mode");
                };
                Some(self.media.set_mode(&self.state, id, mode).await)
            }
            _ => None,
        };
        if let Some(result) = policy_result {
            return session_controls::preference(&request.operation, result);
        }
        let player_id = match request.player_id {
            Some(player_id) => Some(player_id),
            None => self.state.read(|s| s.media.active_player.clone()).await,
        };
        if request.operation == "seek" {
            let Some(offset_seconds) = request.offset_seconds else {
                return ErrorCode::Validation.reply("media.seek requires offset_seconds");
            };
            return session_controls::seek(
                &request.operation,
                offset_seconds,
                self.media.seek(player_id.as_deref(), offset_seconds).await,
            );
        }
        session_controls::playback(
            &request.operation,
            self.media
                .operation(player_id.as_deref(), &request.operation)
                .await,
        )
    }
    pub(super) async fn focus_workspace(&self, params: Value) -> Value {
        let request = request!(params, WorkspaceRequest, "workspace.focus");
        session_controls::workspace(
            &request,
            self.hyprland
                .focus_workspace(request.workspace_id, request.on_current_monitor)
                .await,
        )
    }
}
