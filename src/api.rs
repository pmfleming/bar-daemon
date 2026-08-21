use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
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

mod battery;

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
    notifications: Arc<NotificationService>,
}

impl ApiService {
    pub fn new(
        state: StateStore,
        activity: Arc<ActivityService>,
        notifications: Arc<NotificationService>,
    ) -> Self {
        Self {
            state,
            activity,
            hyprland: Arc::new(HyprlandClient::default()),
            audio_effects: Arc::new(Mutex::new(())),
            brightness_effects: Arc::new(Mutex::new(())),
            notifications,
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
            "battery.setThresholds" => self.battery_set_thresholds(params).await,
            "battery.setProtection" => self.battery_set_protection(params).await,
            "battery.chargeOnce" => self.battery_charge_once().await,
            "battery.setAlertPolicy" => self.battery_set_alert_policy(params).await,
            "powerProfile.set" => self.power_profile_set(params).await,
            "notifications.togglePanel" => self.notification_action(false).await,
            "notifications.toggleDnd" => self.notification_action(true).await,
            "notifications.setDnd" => self.notification_set_dnd(params).await,
            "notifications.list" => self.notification_list(params).await,
            "notifications.dismiss" => self.notification_dismiss(params).await,
            "notifications.clear" => self.notification_clear().await,
            "notifications.invokeAction" => self.notification_invoke_action(params).await,
            "notifications.reply" => self.notification_reply(params).await,
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
            self.notifications
                .toggle_dnd()
                .await
                .map(|enabled| json!({ "operation": "toggle-dnd", "enabled": enabled }))
        } else {
            self.notifications
                .toggle_panel()
                .await
                .map(|()| json!({ "operation": "toggle-panel" }))
        };
        match result {
            Ok(operation) => success(operation),
            Err(error_value) => error("notification-operation-failed", error_value.to_string()),
        }
    }

    async fn notification_set_dnd(&self, params: Value) -> Value {
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error("validation-error", "notifications.setDnd requires enabled");
        };
        let Some(notifications) = self.notifications.native_engine() else {
            return error(
                "native-notifications-required",
                "native notifications are disabled",
            );
        };
        notifications.set_dnd(enabled).await;
        success(json!({ "notifications": self.state.snapshot().await.notifications }))
    }

    async fn notification_list(&self, params: Value) -> Value {
        let Some(notifications) = self.notifications.native_engine() else {
            return error(
                "native-notifications-required",
                "native notifications are disabled",
            );
        };
        let before = match params.get("before_history_id") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_i64() {
                Some(value) if value > 0 => Some(value),
                _ => {
                    return error(
                        "validation-error",
                        "before_history_id must be positive or null",
                    );
                }
            },
        };
        let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50);
        let Ok(limit) = usize::try_from(limit.min(200)) else {
            return error("validation-error", "notification history limit is invalid");
        };
        match notifications.history(before, limit).await {
            Ok(history) => success(json!({ "notification_history": history })),
            Err(error_value) => error("notification-history-failed", error_value.to_string()),
        }
    }

    async fn notification_dismiss(&self, params: Value) -> Value {
        let Some(id) = notification_id(&params) else {
            return error(
                "validation-error",
                "notifications.dismiss requires a positive id",
            );
        };
        let Some(notifications) = self.notifications.native_engine() else {
            return error(
                "native-notifications-required",
                "native notifications are disabled",
            );
        };
        if notifications.dismiss(id).await {
            success(json!({ "operation": "dismiss", "id": id }))
        } else {
            error(
                "notification-not-found",
                format!("notification {id} is not active"),
            )
        }
    }

    async fn notification_clear(&self) -> Value {
        let Some(notifications) = self.notifications.native_engine() else {
            return error(
                "native-notifications-required",
                "native notifications are disabled",
            );
        };
        success(json!({ "operation": "clear", "closed": notifications.clear().await }))
    }

    async fn notification_invoke_action(&self, params: Value) -> Value {
        let Some(id) = notification_id(&params) else {
            return error(
                "validation-error",
                "notifications.invokeAction requires a positive id",
            );
        };
        let Some(action_key) = params.get("action_key").and_then(Value::as_str) else {
            return error(
                "validation-error",
                "notifications.invokeAction requires action_key",
            );
        };
        let token = params
            .get("activation_token")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(notifications) = self.notifications.native_engine() else {
            return error(
                "native-notifications-required",
                "native notifications are disabled",
            );
        };
        if notifications.invoke_action(id, action_key, token).await {
            success(json!({ "operation": "invoke-action", "id": id, "action_key": action_key }))
        } else {
            error(
                "notification-action-not-found",
                "notification or action is unavailable",
            )
        }
    }

    async fn notification_reply(&self, params: Value) -> Value {
        let Some(id) = notification_id(&params) else {
            return error(
                "validation-error",
                "notifications.reply requires a positive id",
            );
        };
        let Some(text) = params.get("text").and_then(Value::as_str) else {
            return error("validation-error", "notifications.reply requires text");
        };
        let Some(notifications) = self.notifications.native_engine() else {
            return error(
                "native-notifications-required",
                "native notifications are disabled",
            );
        };
        if notifications.reply(id, text).await {
            success(json!({ "operation": "reply", "id": id }))
        } else {
            error(
                "notification-not-found",
                format!("notification {id} is not active"),
            )
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

fn notification_id(params: &Value) -> Option<u32> {
    params
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .filter(|id| *id > 0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::ApiService;
    use crate::{
        activity::{
            ActivityService,
            notifications::{
                engine::NotificationEngine,
                model::{IncomingNotification, NotificationHints},
                service::NotificationService,
            },
        },
        state::StateStore,
    };

    async fn api() -> ApiService {
        let state = StateStore::default();
        let notifications = NotificationService::swaync();
        let activity = ActivityService::new(state.clone(), notifications.sink()).await;
        ApiService::new(state, activity, notifications)
    }

    async fn native_api() -> (ApiService, Arc<NotificationEngine>) {
        let state = StateStore::default();
        let notifications = NotificationEngine::new(state.clone()).await;
        let service = NotificationService::native(Arc::clone(&notifications));
        let activity = ActivityService::new(state.clone(), service.sink()).await;
        (ApiService::new(state, activity, service), notifications)
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
    async fn controls_native_notifications() {
        let (api, notifications) = native_api().await;
        let id = notifications
            .notify(
                0,
                IncomingNotification {
                    app_name: "test".into(),
                    app_icon: String::new(),
                    summary: "summary".into(),
                    body: String::new(),
                    actions: Vec::new(),
                    hints: NotificationHints::default(),
                    expire_timeout: 0,
                },
            )
            .await
            .unwrap();
        let dnd = api
            .dispatch("notifications.setDnd", json!({ "enabled": true }))
            .await;
        assert_eq!(dnd["data"]["notifications"]["dnd"], true);
        let dismissed = api
            .dispatch("notifications.dismiss", json!({ "id": id }))
            .await;
        assert_eq!(dismissed["ok"], true);
        assert!(notifications.active().await.is_empty());
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
