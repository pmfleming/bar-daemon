use std::sync::Arc;

use super::{error, success};
use crate::{
    audio, brightness::BrightnessService, hyprland::HyprlandClient, media::MediaService, power,
    sleep, state::StateStore, updates,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

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
}

#[derive(Deserialize)]
struct WorkspaceRequest {
    workspace_id: i64,
    #[serde(default)]
    on_current_monitor: bool,
}

#[derive(Clone, Copy)]
enum AudioCommand {
    Adjust(i16),
    SetMuted(Option<bool>),
    SetInputMuted(Option<bool>),
}

type AudioReply = Option<oneshot::Sender<Result<crate::model::AudioState, String>>>;

#[derive(Clone)]
struct AudioController {
    sender: mpsc::Sender<(AudioCommand, AudioReply)>,
}

impl AudioController {
    fn new(state: StateStore) -> Self {
        let (sender, receiver) = mpsc::channel(64);
        let runtime = tokio::runtime::Handle::current();
        if let Err(error) = std::thread::Builder::new()
            .name("bar-pipewire-control".into())
            .spawn(move || run_audio_controller(receiver, state, runtime))
        {
            tracing::error!(%error, "could not start PipeWire controller");
        }
        Self { sender }
    }

    async fn execute(&self, command: AudioCommand) -> Result<crate::model::AudioState, String> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send((command, Some(reply)))
            .await
            .map_err(|_| "audio controller stopped".to_string())?;
        response
            .await
            .map_err(|_| "audio controller stopped before replying".to_string())?
    }
}

#[derive(Clone)]
pub(super) struct DesktopEffects {
    state: StateStore,
    hyprland: Arc<HyprlandClient>,
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
            hyprland: Arc::new(HyprlandClient::default()),
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
                    json!({"power_sleep": self.state.snapshot().await.power_sleep, "operation": action}),
                )
            }
            Err(value) => error("power-sleep-operation-failed", format!("{value:#}")),
        }
    }
    pub(super) async fn set_keep_awake(&self, params: Value) -> Value {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct KeepAwakeRequest {
            enabled: bool,
        }
        let request = request!(params, KeepAwakeRequest, "powerSleep.setKeepAwake");
        match sleep::set_keep_awake(request.enabled).await {
            Ok(state) => {
                self.state.update_power_sleep(state).await;
                success(json!({"power_sleep": self.state.snapshot().await.power_sleep}))
            }
            Err(value) => error("keep-awake-failed", format!("{value:#}")),
        }
    }

    pub(super) async fn display_layout(&self, method: &str, params: Value) -> Value {
        match crate::display_policy::layout_action(method, params, &self.state).await {
            Ok(state) => success(json!({"display_policy": state})),
            Err(value) => error("display-layout-failed", format!("{value:#}")),
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

    pub(super) async fn idle_sleep(&self, params: Value) -> Value {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct IdleRequest {
            sleep_minutes: u32,
            generation: String,
        }
        let request = request!(params, IdleRequest, "powerSleep.idle");
        match crate::sleep_policy::idle_sleep(
            request.sleep_minutes,
            &request.generation,
            &self.state,
        )
        .await
        {
            Ok(state) => {
                self.state.update_power_sleep(state).await;
                success(json!({"power_sleep": self.state.snapshot().await.power_sleep}))
            }
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
        let battery = self.state.snapshot().await.battery;
        self.power_profile_result(power::set_profile(&request.profile, &battery).await)
            .await
    }
    pub(super) async fn power_profile_resume_automatic(&self) -> Value {
        let battery = self.state.snapshot().await.battery;
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
        if request.operation == "cycle" {
            return match self.media.cycle(&self.state).await {
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
            None => self.state.snapshot().await.media.active_player,
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

fn run_audio_controller(
    mut receiver: mpsc::Receiver<(AudioCommand, AudioReply)>,
    state: StateStore,
    runtime: tokio::runtime::Handle,
) {
    let mut connection = audio::AudioConnection::new().ok();
    while let Some(first) = receiver.blocking_recv() {
        // Start the first key immediately. Coalesce only requests already
        // queued (including repeats received during the previous transaction).
        // All PipeWire objects stay on this thread and reuse one connection.
        let mut pending = vec![first];
        while let Ok(command) = receiver.try_recv() {
            pending.push(command);
        }
        let mut index = 0;
        while index < pending.len() {
            if matches!(pending[index].0, AudioCommand::Adjust(_)) {
                let start = index;
                let (end, delta) = accumulated_adjustment(&pending, start);
                index = end;
                let result = execute_audio(&mut connection, AudioCommand::Adjust(delta));
                runtime.block_on(publish_audio_result(
                    &state,
                    &mut pending[start..index],
                    result,
                ));
                continue;
            }
            let result = execute_audio(&mut connection, pending[index].0);
            runtime.block_on(publish_audio_result(
                &state,
                &mut pending[index..=index],
                result,
            ));
            index += 1;
        }
    }
}

fn execute_audio(
    connection: &mut Option<audio::AudioConnection>,
    command: AudioCommand,
) -> Result<crate::model::AudioState, String> {
    let result = (|| {
        if connection.is_none() {
            *connection = Some(audio::AudioConnection::new()?);
        }
        let connection = connection.as_ref().expect("connection initialized");
        match command {
            AudioCommand::Adjust(delta) => connection.adjust(delta),
            AudioCommand::SetMuted(muted) => connection.set_muted(muted),
            AudioCommand::SetInputMuted(muted) => connection.set_input_muted(muted),
        }
    })();
    if result.is_err() {
        // Reconnect for the NEXT request, never replay a possibly applied
        // adjustment/toggle after a disconnect or verification failure.
        *connection = None;
    }
    result.map_err(|error: anyhow::Error| error.to_string())
}

fn accumulated_adjustment(commands: &[(AudioCommand, AudioReply)], start: usize) -> (usize, i16) {
    let AudioCommand::Adjust(first) = commands[start].0 else {
        return (start, 0);
    };
    let mut end = start;
    let mut delta = 0_i32;
    while end < commands.len() {
        let AudioCommand::Adjust(value) = commands[end].0 else {
            break;
        };
        // Opposite directions must retain their clamp ordering (e.g. at 100%).
        if value.signum() != first.signum() {
            break;
        }
        delta = delta.saturating_add(i32::from(value));
        end += 1;
    }
    (
        end,
        delta.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
    )
}

async fn publish_audio_result(
    state: &StateStore,
    commands: &mut [(AudioCommand, AudioReply)],
    result: Result<crate::model::AudioState, String>,
) {
    if let Ok(audio) = &result {
        state.update_audio(audio.clone()).await;
    }
    for (_, reply) in commands {
        let value = match &result {
            Ok(audio) => Ok(audio.clone()),
            Err(error) => Err(error.clone()),
        };
        if let Some(reply) = reply.take() {
            let _ = reply.send(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioCommand, accumulated_adjustment, publish_audio_result};
    use crate::{model::AudioState, state::StateStore};
    use tokio::sync::oneshot;

    #[test]
    fn coalesces_queued_repeats_without_reordering_mutes_or_direction_changes() {
        let commands = [
            (AudioCommand::Adjust(5), None),
            (AudioCommand::Adjust(5), None),
            (AudioCommand::Adjust(-5), None),
            (AudioCommand::SetMuted(None), None),
            (AudioCommand::Adjust(-5), None),
            (AudioCommand::Adjust(-5), None),
        ];
        assert_eq!(accumulated_adjustment(&commands, 0), (2, 10));
        assert_eq!(accumulated_adjustment(&commands, 2), (3, -5));
        assert_eq!(accumulated_adjustment(&commands, 4), (6, -10));
        let commands = [
            (AudioCommand::Adjust(i16::MAX), None),
            (AudioCommand::Adjust(i16::MAX), None),
        ];
        assert_eq!(accumulated_adjustment(&commands, 0), (2, i16::MAX));
    }

    #[tokio::test]
    async fn coalesced_requests_all_receive_the_confirmed_state() {
        let state = StateStore::default();
        let (first, first_reply) = oneshot::channel();
        let (second, second_reply) = oneshot::channel();
        let mut commands = [
            (AudioCommand::Adjust(5), Some(first)),
            (AudioCommand::Adjust(5), Some(second)),
        ];
        let audio = AudioState {
            available: true,
            volume_percent: 60,
            ..AudioState::default()
        };
        publish_audio_result(&state, &mut commands, Ok(audio.clone())).await;
        assert_eq!(first_reply.await.unwrap().unwrap(), audio);
        assert_eq!(second_reply.await.unwrap().unwrap(), audio);
        assert_eq!(state.snapshot().await.audio, audio);
    }
}
