use std::sync::Arc;

use super::{error, success};
use crate::{
    audio, brightness, hyprland::HyprlandClient, media::MediaService, power, sleep,
    state::StateStore, updates,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, sleep};

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
        tokio::spawn(run_audio_controller(receiver, state));
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
    brightness: Arc<Mutex<()>>,
    media: MediaService,
}

impl DesktopEffects {
    pub(super) fn new(state: StateStore, media: MediaService) -> Self {
        Self {
            state: state.clone(),
            hyprland: Arc::new(HyprlandClient::default()),
            audio: AudioController::new(state),
            brightness: Arc::new(Mutex::new(())),
            media,
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

async fn run_audio_controller(
    mut receiver: mpsc::Receiver<(AudioCommand, AudioReply)>,
    state: StateStore,
) {
    while let Some(first) = receiver.recv().await {
        // Key-repeat requests normally arrive within one scheduler turn. A
        // tiny collection window lets one PipeWire transaction apply the
        // accumulated delta instead of queueing obsolete probes.
        sleep(Duration::from_millis(8)).await;
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
                let result = audio::adjust(delta)
                    .await
                    .map_err(|error| error.to_string());
                publish_audio_result(&state, &mut pending[start..index], result).await;
                continue;
            }
            let result = match pending[index].0 {
                AudioCommand::SetMuted(value) => audio::set_muted(value).await,
                AudioCommand::SetInputMuted(value) => audio::set_input_muted(value).await,
                AudioCommand::Adjust(_) => unreachable!(),
            }
            .map_err(|error| error.to_string());
            publish_audio_result(&state, &mut pending[index..=index], result).await;
            index += 1;
        }
    }
}

fn accumulated_adjustment(commands: &[(AudioCommand, AudioReply)], start: usize) -> (usize, i16) {
    let mut end = start;
    let mut delta = 0_i32;
    while end < commands.len() {
        let AudioCommand::Adjust(value) = commands[end].0 else {
            break;
        };
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
    use crate::{
        media::MediaService,
        model::{MediaPlayer, MediaState, PowerProfileState},
        state::StateStore,
    };
    use serde_json::json;

    use super::{AudioCommand, DesktopEffects, accumulated_adjustment};

    #[test]
    fn adjacent_volume_key_requests_are_coalesced() {
        let commands = vec![
            (AudioCommand::Adjust(2), None),
            (AudioCommand::Adjust(3), None),
            (AudioCommand::SetMuted(Some(true)), None),
            (AudioCommand::Adjust(-1), None),
        ];
        assert_eq!(accumulated_adjustment(&commands, 0), (2, 5));
        assert_eq!(accumulated_adjustment(&commands, 3), (4, -1));
    }

    #[tokio::test]
    async fn cycles_media_players_without_calling_mpris() {
        let store = StateStore::default();
        store
            .update_media(MediaState {
                available: true,
                active_player: Some("first".into()),
                players: vec![
                    MediaPlayer {
                        id: "first".into(),
                        ..MediaPlayer::default()
                    },
                    MediaPlayer {
                        id: "second".into(),
                        ..MediaPlayer::default()
                    },
                ],
                error: None,
            })
            .await;
        let effects = DesktopEffects::new(store.clone(), MediaService::default());

        let response = effects
            .media_operation(json!({ "operation": "cycle" }))
            .await;

        assert_eq!(response["data"]["player_id"], "second");
        assert_eq!(
            store.snapshot().await.media.active_player.as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn validates_media_seek_before_calling_mpris() {
        let store = StateStore::default();
        let effects = DesktopEffects::new(store, MediaService::default());

        let missing = effects
            .media_operation(json!({ "operation": "seek" }))
            .await;
        let out_of_range = effects
            .media_operation(json!({ "operation": "seek", "offset_seconds": 86401 }))
            .await;

        assert_eq!(missing["error"]["code"], "validation-error");
        assert_eq!(out_of_range["error"]["code"], "media-operation-failed");
    }

    #[tokio::test]
    async fn successful_power_profile_effect_updates_the_snapshot() {
        let store = StateStore::default();
        let effects = DesktopEffects::new(store.clone(), MediaService::default());
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
