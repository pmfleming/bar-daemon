use std::sync::Arc;

use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
    media::MediaService,
    protocol,
    state::StateStore,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use shelllist_daemon_core::{ApiError as EnvelopeError, ApiIdentity};

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

use self::{
    activity::ActivityApi, battery::BatteryApi, effects::DesktopEffects,
    notifications::NotificationApi,
};

pub(crate) use protocol::{NAME as PROTOCOL, VERSION};
pub(crate) const BUS_NAME: &str = "org.laufan.BarDaemon";
pub(crate) const OBJECT_PATH: &str = "/org/laufan/BarDaemon";
pub(crate) const INTERFACE: &str = "org.laufan.BarDaemon1";
const API: ApiIdentity = ApiIdentity::new(PROTOCOL, VERSION as u32);

pub(crate) fn success(data: Value) -> Value {
    shelllist_daemon_core::success(API, data)
}
pub(crate) fn error(code: &str, message: impl Into<String>) -> Value {
    shelllist_daemon_core::error(API, EnvelopeError::new(code, message))
}

fn decode_request<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, Value> {
    serde_json::from_value(params).map_err(|value| {
        error(
            "validation-error",
            format!("{method} parameters are invalid: {value}"),
        )
    })
}

#[derive(Clone)]
pub(crate) struct ApiService {
    state: StateStore,
    activity: ActivityApi,
    battery: BatteryApi,
    effects: DesktopEffects,
    notifications: NotificationApi,
}

impl ApiService {
    pub(crate) fn new(
        state: StateStore,
        activity: Arc<ActivityService>,
        notifications: Arc<NotificationService>,
        media: MediaService,
    ) -> Self {
        Self {
            activity: ActivityApi::new(state.clone(), activity),
            battery: BatteryApi::new(state.clone()),
            effects: DesktopEffects::new(state.clone(), media),
            notifications: NotificationApi::new(state.clone(), notifications),
            state,
        }
    }

    pub(crate) async fn dispatch(&self, method: &str, params: Value) -> Value {
        match method {
            "bar.snapshot" => success(json!({ "snapshot": self.state.snapshot().await })),
            "activity.queryRange" => self.activity.activity_query_range(params).await,
            "activity.refresh" => self.activity.activity_refresh().await,
            "todos.create" => self.activity.todo_create(params).await,
            "todos.complete" => self.activity.todo_complete(params).await,
            "todos.delete" => self.activity.todo_delete(params).await,
            "workspace.focus" => self.effects.focus_workspace(params).await,
            "media.operation" => self.effects.media_operation(params).await,
            "audio.adjust" => self.effects.audio_adjust(params).await,
            "audio.setMuted" => self.effects.audio_set_muted(params).await,
            "audio.setInputMuted" => self.effects.audio_set_input_muted(params).await,
            "brightness.adjust" => self.effects.brightness_adjust(params).await,
            "brightness.set" => self.effects.brightness_set(params).await,
            "battery.history" => self.battery.battery_history(),
            "battery.setThresholds" => self.battery.battery_set_thresholds(params).await,
            "battery.setProtection" => self.battery.battery_set_protection(params).await,
            "battery.chargeOnce" => self.battery.battery_charge_once(params).await,
            "battery.setChargingInhibited" => {
                self.battery.battery_set_charging_inhibited(params).await
            }
            "battery.startCalibration" => self.battery.battery_start_calibration(params).await,
            "battery.cancelCalibration" => self.battery.battery_cancel_calibration(params).await,
            "battery.setAlertPolicy" => self.battery.battery_set_alert_policy(params).await,
            "powerProfile.set" => self.effects.power_profile_set(params).await,
            "powerProfile.resumeAutomatic" => self.effects.power_profile_resume_automatic().await,
            "powerProfile.setBatteryAware" => {
                self.effects.power_profile_set_battery_aware(params).await
            }
            "powerProfile.setActionEnabled" => {
                self.effects.power_profile_set_action_enabled(params).await
            }
            "powerSleep.lock" => self.effects.power_sleep_action("lock").await,
            "powerSleep.suspend" => self.effects.power_sleep_action("suspend").await,
            "powerSleep.hibernate" => self.effects.power_sleep_action("hibernate").await,
            "notifications.togglePanel" => self.notifications.notification_action(false).await,
            "notifications.toggleDnd" => self.notifications.notification_action(true).await,
            "notifications.setDnd" => self.notifications.notification_set_dnd(params).await,
            "notifications.list" => self.notifications.notification_list(params).await,
            "notifications.dismiss" => self.notifications.notification_dismiss(params).await,
            "notifications.clear" => self.notifications.notification_clear().await,
            "notifications.clearGroup" => self.notifications.notification_clear_group(params).await,
            "notifications.snooze" => self.notifications.notification_snooze(params).await,
            "notifications.invokeAction" => {
                self.notifications.notification_invoke_action(params).await
            }
            "notifications.reply" => self.notifications.notification_reply(params).await,
            "updates.refresh" => self.effects.updates_refresh().await,
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
        activity::{ActivityService, notifications::service::NotificationService},
        media::MediaService,
        state::StateStore,
    };
    use serde_json::json;
    async fn api() -> ApiService {
        let state = StateStore::default();
        let notifications = NotificationService::swaync();
        let activity = ActivityService::new(state.clone(), notifications.sink()).await;
        ApiService::new(state, activity, notifications, MediaService::default())
    }
    #[tokio::test]
    async fn returns_versioned_snapshot() {
        let response = api().await.dispatch("bar.snapshot", json!({})).await;
        assert_eq!(response["protocol"], "bar-api");
        assert_eq!(response["version"], 1);
        assert_eq!(response["ok"], true);
    }
}
