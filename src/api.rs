use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    activity::{ActivityService, notifications},
    audio, brightness,
    hyprland::HyprlandClient,
    media, power,
    state::StateStore,
    updates,
};

pub const PROTOCOL: &str = "bar-api";
pub const VERSION: u8 = 1;
pub const BUS_NAME: &str = "org.laufan.BarDaemon";
pub const OBJECT_PATH: &str = "/org/laufan/BarDaemon";
pub const INTERFACE: &str = "org.laufan.BarDaemon1";

pub fn success(data: Value) -> Value {
    json!({ "protocol": PROTOCOL, "version": VERSION, "ok": true, "data": data })
}

pub fn error(code: &str, message: impl Into<String>) -> Value {
    json!({
        "protocol": PROTOCOL,
        "version": VERSION,
        "ok": false,
        "error": { "code": code, "message": message.into() }
    })
}

#[derive(Clone)]
pub struct ApiService {
    state: StateStore,
    activity: Arc<ActivityService>,
    hyprland: Arc<HyprlandClient>,
    audio_effects: Arc<Mutex<()>>,
    brightness_effects: Arc<Mutex<()>>,
}

impl ApiService {
    pub fn new(state: StateStore, activity: Arc<ActivityService>) -> Self {
        Self {
            state,
            activity,
            hyprland: Arc::new(HyprlandClient::default()),
            audio_effects: Arc::new(Mutex::new(())),
            brightness_effects: Arc::new(Mutex::new(())),
        }
    }

    pub async fn dispatch(&self, method: &str, params: Value) -> Value {
        match method {
            "bar.snapshot" => success(json!({ "snapshot": self.state.snapshot().await })),
            "activity.queryRange" => self.activity_query_range(params).await,
            "activity.refresh" => self.activity_refresh().await,
            "todos.create" => self.todo_create(params).await,
            "todos.complete" => self.todo_complete(params).await,
            "todos.delete" => self.todo_delete(params).await,
            "workspace.focus" => self.focus_workspace(params).await,
            "media.operation" => self.media_operation(params).await,
            "audio.adjust" => self.audio_adjust(params).await,
            "audio.setMuted" => self.audio_set_muted(params).await,
            "audio.setInputMuted" => self.audio_set_input_muted(params).await,
            "brightness.adjust" => self.brightness_adjust(params).await,
            "brightness.set" => self.brightness_set(params).await,
            "powerProfile.set" => self.power_profile_set(params).await,
            "notifications.togglePanel" => self.notification_action(false).await,
            "notifications.toggleDnd" => self.notification_action(true).await,
            "updates.refresh" => self.updates_refresh().await,
            _ => error(
                "unsupported-method",
                format!("Unsupported bar-api method: {method}"),
            ),
        }
    }

    async fn activity_query_range(&self, params: Value) -> Value {
        let Some(from_unix_ms) = params.get("from_unix_ms").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "activity.queryRange requires integer from_unix_ms",
            );
        };
        let Some(to_unix_ms) = params.get("to_unix_ms").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "activity.queryRange requires integer to_unix_ms",
            );
        };
        match self.activity.query_range(from_unix_ms, to_unix_ms).await {
            Ok(range) => success(json!({ "activity_range": range })),
            Err(error_value) => error("activity-query-failed", error_value.to_string()),
        }
    }

    async fn activity_refresh(&self) -> Value {
        self.activity.refresh().await;
        success(json!({ "activity": self.state.snapshot().await.activity }))
    }

    async fn todo_create(&self, params: Value) -> Value {
        let Some(title) = params.get("title").and_then(Value::as_str) else {
            return error("validation-error", "todos.create requires title");
        };
        let due_unix_ms = match params.get("due_unix_ms") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_i64() {
                Some(value) => Some(value),
                None => return error("validation-error", "due_unix_ms must be integer or null"),
            },
        };
        let due_date = match params.get("due_date") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => return error("validation-error", "due_date must be YYYY-MM-DD or null"),
        };
        let priority = params.get("priority").and_then(Value::as_u64).unwrap_or(0);
        let Ok(priority) = u8::try_from(priority) else {
            return error("validation-error", "todo priority is out of range");
        };
        match self
            .activity
            .create_todo(title.into(), due_unix_ms, due_date, priority)
            .await
        {
            Ok(todo) => success(json!({ "todo": todo })),
            Err(error_value) => error("todo-create-failed", error_value.to_string()),
        }
    }

    async fn todo_complete(&self, params: Value) -> Value {
        let Some(id) = params.get("id").and_then(Value::as_str) else {
            return error("validation-error", "todos.complete requires id");
        };
        let completed = params
            .get("completed")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        match self.activity.complete_todo(id, completed).await {
            Ok(todo) => success(json!({ "todo": todo })),
            Err(error_value) => error("todo-complete-failed", error_value.to_string()),
        }
    }

    async fn todo_delete(&self, params: Value) -> Value {
        let Some(id) = params.get("id").and_then(Value::as_str) else {
            return error("validation-error", "todos.delete requires id");
        };
        match self.activity.delete_todo(id).await {
            Ok(()) => success(json!({ "deleted": id })),
            Err(error_value) => error("todo-delete-failed", error_value.to_string()),
        }
    }

    async fn updates_refresh(&self) -> Value {
        match updates::refresh_default(&self.state).await {
            Ok(state) => success(json!({ "updates": state })),
            Err(error_value) => error("update-refresh-failed", error_value.to_string()),
        }
    }

    async fn notification_action(&self, dnd: bool) -> Value {
        let result = if dnd {
            notifications::toggle_dnd().await
        } else {
            notifications::toggle_panel().await
        };
        match result {
            Ok(()) => {
                success(json!({ "operation": if dnd { "toggle-dnd" } else { "toggle-panel" } }))
            }
            Err(error_value) => error("notification-operation-failed", error_value.to_string()),
        }
    }

    async fn power_profile_set(&self, params: Value) -> Value {
        let Some(profile) = params.get("profile").and_then(Value::as_str) else {
            return error("validation-error", "powerProfile.set requires profile");
        };
        match power::set_profile(profile).await {
            Ok(state) => success(json!({ "power_profile": state })),
            Err(error_value) => error("power-profile-operation-failed", error_value.to_string()),
        }
    }

    async fn brightness_adjust(&self, params: Value) -> Value {
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
                self.state.update_brightness(state.clone()).await;
                success(json!({ "brightness": state }))
            }
            Err(error_value) => error("brightness-operation-failed", error_value.to_string()),
        }
    }

    async fn brightness_set(&self, params: Value) -> Value {
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
                self.state.update_brightness(state.clone()).await;
                success(json!({ "brightness": state }))
            }
            Err(error_value) => error("brightness-operation-failed", error_value.to_string()),
        }
    }

    async fn audio_adjust(&self, params: Value) -> Value {
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
                self.state.update_audio(state.clone()).await;
                success(json!({ "audio": state }))
            }
            Err(error_value) => error("audio-operation-failed", error_value.to_string()),
        }
    }

    async fn audio_set_muted(&self, params: Value) -> Value {
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
                self.state.update_audio(state.clone()).await;
                success(json!({ "audio": state }))
            }
            Err(error_value) => error("audio-operation-failed", error_value.to_string()),
        }
    }

    async fn audio_set_input_muted(&self, params: Value) -> Value {
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
                self.state.update_audio(state.clone()).await;
                success(json!({ "audio": state }))
            }
            Err(error_value) => error("audio-operation-failed", error_value.to_string()),
        }
    }

    async fn media_operation(&self, params: Value) -> Value {
        let Some(operation) = params.get("operation").and_then(Value::as_str) else {
            return error("validation-error", "media.operation requires operation");
        };
        let player_id = params.get("player_id").and_then(Value::as_str);
        match media::operation(player_id, operation).await {
            Ok(player_id) => success(json!({ "operation": operation, "player_id": player_id })),
            Err(error_value) => error("media-operation-failed", error_value.to_string()),
        }
    }

    async fn focus_workspace(&self, params: Value) -> Value {
        let Some(workspace_id) = params.get("workspace_id").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "workspace.focus requires integer workspace_id",
            );
        };
        let on_current_monitor = params
            .get("on_current_monitor")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match self
            .hyprland
            .focus_workspace(workspace_id, on_current_monitor)
            .await
        {
            Ok(()) => success(json!({
                "operation": "workspace.focus",
                "workspace_id": workspace_id,
                "on_current_monitor": on_current_monitor
            })),
            Err(error_value) => error("workspace-focus-failed", error_value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ApiService;
    use crate::{activity::ActivityService, state::StateStore};

    async fn api() -> ApiService {
        let state = StateStore::default();
        let activity = ActivityService::new(state.clone()).await;
        ApiService::new(state, activity)
    }

    #[tokio::test]
    async fn rejects_invalid_workspace_focus() {
        let api = api().await;
        let response = api.dispatch("workspace.focus", json!({})).await;
        assert_eq!(response["error"]["code"], "validation-error");
    }

    #[tokio::test]
    async fn returns_versioned_snapshot() {
        let api = api().await;
        let response = api.dispatch("bar.snapshot", json!({})).await;
        assert_eq!(response["protocol"], "bar-api");
        assert_eq!(response["version"], 1);
        assert_eq!(response["ok"], true);
    }

    #[tokio::test]
    async fn validates_input_mute_without_touching_pipewire() {
        let api = api().await;
        let response = api
            .dispatch("audio.setInputMuted", json!({ "muted": "yes" }))
            .await;
        assert_eq!(response["error"]["code"], "validation-error");
    }
}
