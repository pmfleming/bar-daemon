use super::{ApiService, error, success};
use crate::{audio, brightness, media, power, sleep, updates};
use serde_json::{Value, json};

impl ApiService {
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
    pub(super) async fn power_profile_set(params: Value) -> Value {
        let Some(profile) = params.get("profile").and_then(Value::as_str) else {
            return error("validation-error", "powerProfile.set requires profile");
        };
        match power::set_profile(profile).await {
            Ok(state) => success(json!({"power_profile": state})),
            Err(value) => error("power-profile-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn power_profile_set_battery_aware(params: Value) -> Value {
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error(
                "validation-error",
                "powerProfile.setBatteryAware requires enabled",
            );
        };
        match power::set_battery_aware(enabled).await {
            Ok(state) => success(json!({"power_profile": state})),
            Err(value) => error("power-profile-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn power_profile_set_action_enabled(params: Value) -> Value {
        let Some(action) = params.get("action").and_then(Value::as_str) else {
            return error(
                "validation-error",
                "powerProfile.setActionEnabled requires action",
            );
        };
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error(
                "validation-error",
                "powerProfile.setActionEnabled requires enabled",
            );
        };
        match power::set_action_enabled(action, enabled).await {
            Ok(state) => success(json!({"power_profile": state})),
            Err(value) => error("power-profile-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn brightness_adjust(&self, params: Value) -> Value {
        let Some(delta) = params.get("delta_percent").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "brightness.adjust requires integer delta_percent",
            );
        };
        let Ok(delta) = i16::try_from(delta) else {
            return error(
                "validation-error",
                "brightness delta_percent is out of range",
            );
        };
        let _guard = self.brightness_effects.lock().await;
        match brightness::adjust(delta).await {
            Ok(state) => {
                let response = success(json!({"brightness": state}));
                self.state.update_brightness(state).await;
                response
            }
            Err(value) => error("brightness-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn brightness_set(&self, params: Value) -> Value {
        let Some(percent) = params.get("percent").and_then(Value::as_u64) else {
            return error(
                "validation-error",
                "brightness.set requires integer percent",
            );
        };
        let Ok(percent) = u8::try_from(percent) else {
            return error("validation-error", "brightness percent is out of range");
        };
        let _guard = self.brightness_effects.lock().await;
        match brightness::set(percent).await {
            Ok(state) => {
                let response = success(json!({"brightness": state}));
                self.state.update_brightness(state).await;
                response
            }
            Err(value) => error("brightness-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn audio_adjust(&self, params: Value) -> Value {
        let Some(delta) = params.get("delta_percent").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "audio.adjust requires integer delta_percent",
            );
        };
        let Ok(delta) = i16::try_from(delta) else {
            return error("validation-error", "audio delta_percent is out of range");
        };
        let _guard = self.audio_effects.lock().await;
        match audio::adjust(delta).await {
            Ok(state) => {
                let response = success(json!({"audio": state}));
                self.state.update_audio(state).await;
                response
            }
            Err(value) => error("audio-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn audio_set_muted(&self, params: Value) -> Value {
        let muted = match params.get("muted") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(value)) => Some(*value),
            _ => {
                return error(
                    "validation-error",
                    "audio.setMuted muted must be boolean or null",
                );
            }
        };
        let _guard = self.audio_effects.lock().await;
        match audio::set_muted(muted).await {
            Ok(state) => {
                let response = success(json!({"audio": state}));
                self.state.update_audio(state).await;
                response
            }
            Err(value) => error("audio-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn audio_set_input_muted(&self, params: Value) -> Value {
        let muted = match params.get("muted") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(value)) => Some(*value),
            _ => {
                return error(
                    "validation-error",
                    "audio.setInputMuted muted must be boolean or null",
                );
            }
        };
        let _guard = self.audio_effects.lock().await;
        match audio::set_input_muted(muted).await {
            Ok(state) => {
                let response = success(json!({"audio": state}));
                self.state.update_audio(state).await;
                response
            }
            Err(value) => error("audio-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn media_operation(params: Value) -> Value {
        let Some(operation) = params.get("operation").and_then(Value::as_str) else {
            return error("validation-error", "media.operation requires operation");
        };
        let player = params.get("player_id").and_then(Value::as_str);
        match media::operation(player, operation).await {
            Ok(player_id) => success(json!({"operation": operation, "player_id": player_id})),
            Err(value) => error("media-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn focus_workspace(&self, params: Value) -> Value {
        let Some(id) = params.get("workspace_id").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "workspace.focus requires integer workspace_id",
            );
        };
        let current = params
            .get("on_current_monitor")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match self.hyprland.focus_workspace(id, current).await {
            Ok(()) => success(
                json!({"operation": "workspace.focus", "workspace_id": id, "on_current_monitor": current}),
            ),
            Err(value) => error("workspace-focus-failed", value.to_string()),
        }
    }
}
