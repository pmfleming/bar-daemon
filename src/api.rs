use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
    hyprland::HyprlandClient,
    protocol,
    state::StateStore,
};

macro_rules! request {
    ($params:expr, $request:ty, $method:literal) => {
        match super::decode_request::<$request>($params, $method) {
            Ok(request) => request,
            Err(response) => return response,
        }
    };
}

mod activity;
mod battery;
mod effects;
mod notifications;

pub use protocol::{NAME as PROTOCOL, VERSION};
pub const BUS_NAME: &str = "org.laufan.BarDaemon";
pub const OBJECT_PATH: &str = "/org/laufan/BarDaemon";
pub const INTERFACE: &str = "org.laufan.BarDaemon1";

pub fn success(data: Value) -> Value {
    json!({ "protocol": PROTOCOL, "version": VERSION, "ok": true, "data": data })
}
pub fn error(code: &str, message: impl Into<String>) -> Value {
    json!({ "protocol": PROTOCOL, "version": VERSION, "ok": false, "error": { "code": code, "message": message.into() } })
}

fn decode_request<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, Value> {
    serde_json::from_value(params).map_err(|value| {
        error(
            "validation-error",
            format!("{method} parameters are invalid: {value}"),
        )
    })
}

fn result<T: Serialize>(value: anyhow::Result<T>, key: &'static str, code: &str) -> Value {
    match value {
        Ok(value) => success(json!({(key): value})),
        Err(value) => error(code, value.to_string()),
    }
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
            "media.operation" => Self::media_operation(params).await,
            "audio.adjust" => self.audio_adjust(params).await,
            "audio.setMuted" => self.audio_set_muted(params).await,
            "audio.setInputMuted" => self.audio_set_input_muted(params).await,
            "brightness.adjust" => self.brightness_adjust(params).await,
            "brightness.set" => self.brightness_set(params).await,
            "battery.history" => self.battery_history(),
            "battery.setThresholds" => self.battery_set_thresholds(params).await,
            "battery.setProtection" => self.battery_set_protection(params).await,
            "battery.chargeOnce" => self.battery_charge_once(params).await,
            "battery.setChargingInhibited" => self.battery_set_charging_inhibited(params).await,
            "battery.startCalibration" => self.battery_start_calibration(params).await,
            "battery.cancelCalibration" => self.battery_cancel_calibration(params).await,
            "battery.setAlertPolicy" => self.battery_set_alert_policy(params).await,
            "powerProfile.set" => Self::power_profile_set(params).await,
            "powerProfile.setBatteryAware" => Self::power_profile_set_battery_aware(params).await,
            "powerProfile.setActionEnabled" => Self::power_profile_set_action_enabled(params).await,
            "powerSleep.lock" => self.power_sleep_action("lock").await,
            "powerSleep.suspend" => self.power_sleep_action("suspend").await,
            "powerSleep.hibernate" => self.power_sleep_action("hibernate").await,
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
}

#[cfg(test)]
mod tests {
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
    use serde_json::json;
    use std::sync::Arc;

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
        let response = api().await.dispatch("workspace.focus", json!({})).await;
        assert_eq!(response["error"]["code"], "validation-error");
    }
    #[tokio::test]
    async fn returns_versioned_snapshot() {
        let response = api().await.dispatch("bar.snapshot", json!({})).await;
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
    async fn rejects_unsupported_media_operation_without_accessing_dbus() {
        let response = api()
            .await
            .dispatch("media.operation", json!({ "operation": "shuffle" }))
            .await;
        assert_eq!(response["error"]["code"], "media-operation-failed");
    }

    #[tokio::test]
    async fn validates_todo_priority_range() {
        let response = api()
            .await
            .dispatch("todos.create", json!({ "title": "test", "priority": 300 }))
            .await;
        assert_eq!(response["error"]["code"], "validation-error");
    }

    #[tokio::test]
    async fn validates_input_mute_without_touching_pipewire() {
        let response = api()
            .await
            .dispatch("audio.setInputMuted", json!({ "muted": "yes" }))
            .await;
        assert_eq!(response["error"]["code"], "validation-error");
    }
}
