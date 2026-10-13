use std::sync::Arc;

use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
    media::MediaService,
    protocol,
    state::StateStore,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
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
pub(crate) mod audio;
mod battery;
mod effects;
mod notifications;
mod sleep;

use self::{
    activity::ActivityApi, battery::BatteryApi, effects::DesktopEffects,
    notifications::NotificationApi,
};

pub(crate) use protocol::{NAME as PROTOCOL, VERSION};
pub(crate) const BUS_NAME: &str = "org.laufan.BarDaemon";
pub(crate) const OBJECT_PATH: &str = "/org/laufan/BarDaemon";
pub(crate) const INTERFACE: &str = "org.laufan.BarDaemon2";
const API: ApiIdentity = ApiIdentity::new(PROTOCOL, VERSION as u32);

pub(crate) fn success(data: Value) -> Value {
    shelllist_daemon_core::success(API, data)
}
pub(crate) fn error(code: &str, message: impl Into<String>) -> Value {
    shelllist_daemon_core::error(API, EnvelopeError::new(code, message))
}

fn unsupported_method(method: &str) -> Value {
    error(
        "unsupported-method",
        format!("Unsupported bar-api method: {method}"),
    )
}

pub(crate) fn decode_request<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, Value> {
    serde_json::from_value(params).map_err(|value| {
        error(
            "validation-error",
            format!("{method} parameters are invalid: {value}"),
        )
    })
}

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
        brightness: crate::brightness::BrightnessService,
    ) -> Self {
        Self {
            activity: ActivityApi::new(state.clone(), activity),
            battery: BatteryApi::new(state.clone()),
            effects: DesktopEffects::new(state.clone(), media, brightness),
            notifications: NotificationApi::new(notifications),
            state,
        }
    }

    async fn snapshot(&self, params: Value) -> Value {
        static REQUESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let snapshot_requests_total =
            REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let started = std::time::Instant::now();
        let result = match params.get("streams") {
            Some(streams) => self.project_snapshot(streams).await,
            None => self.state.read(protocol::read_wire::snapshot).await,
        };
        tracing::debug!(snapshot_requests_total, streams = ?params.get("streams"), latency_us = started.elapsed().as_micros() as u64,
            payload_bytes = result.to_string().len(), "bar snapshot request");
        result
    }

    async fn project_snapshot(&self, streams: &Value) -> Value {
        let streams = <Vec<&str> as serde::Deserialize>::deserialize(streams);
        let Some(streams) = streams
            .ok()
            .filter(|streams| streams.iter().all(|s| protocol::STREAMS.contains(s)))
        else {
            return error(
                "validation-error",
                "streams must be a list of registered bar streams",
            );
        };
        self.state
            .read(|snapshot| {
                let projection: serde_json::Map<String, Value> = streams
                    .iter()
                    .map(|stream| {
                        (
                            protocol::read_wire::snapshot_key(stream).into(),
                            protocol::read_wire::initial_stream_data(stream, snapshot),
                        )
                    })
                    .collect();
                success(serde_json::json!({"snapshot": projection}))
            })
            .await
    }

    pub(crate) async fn dispatch(&self, method: &str, params: Value) -> Value {
        let response = match method {
            "bar.snapshot" => self.snapshot(params).await,
            "activity.queryRange" => self.activity.activity_query_range(params).await,
            "activity.refresh" => self.activity.activity_refresh().await,
            "todos.create" => self.activity.todo_create(params).await,
            "todos.complete" => self.activity.todo_complete(params).await,
            "todos.delete" => self.activity.todo_delete(params).await,
            "workspace.focus" => self.effects.focus_workspace(params).await,
            "media.operation" => self.effects.media_operation(params).await,
            method if audio::Method::parse(method).is_some() => {
                audio::dispatch(self, method, params).await
            }
            "brightness.adjust" => self.effects.brightness_adjust(params).await,
            "brightness.set" => self.effects.brightness_set(params).await,
            method if method.starts_with("battery.") => self.battery.dispatch(method, params).await,
            "powerProfile.set" => self.effects.power_profile_set(params).await,
            "powerProfile.resumeAutomatic" => self.effects.power_profile_resume_automatic().await,
            "powerProfile.setBatteryAware" => {
                self.effects.power_profile_set_battery_aware(params).await
            }
            "powerProfile.setActionEnabled" => {
                self.effects.power_profile_set_action_enabled(params).await
            }
            method if method.starts_with("powerSleep.") => {
                sleep::dispatch(&self.state, method, params).await
            }
            "displayPolicy.set" => self.effects.display_policy_set(params).await,
            "displayFocus.set" | "displayFocus.reset" => {
                self.effects.display_focus(method, params).await
            }
            "displayLayout.preview" | "displayLayout.confirm" | "displayLayout.revert" => {
                self.effects.display_layout(method, params).await
            }
            method if method.starts_with("notifications.") => {
                self.notifications.dispatch(method, params).await
            }
            "updates.refresh" => self.effects.updates_refresh().await,
            _ => unsupported_method(method),
        };
        self.state.fence_reply(response)
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
        let activity = ActivityService::new(state.clone()).await;
        let brightness = crate::brightness::BrightnessService::new(state.clone());
        ApiService::new(
            state,
            activity,
            notifications,
            MediaService::default(),
            brightness,
        )
    }

    #[tokio::test]
    async fn routing_preserves_backend_and_validation_errors() {
        let api = api().await;
        for params in [
            json!({}),
            json!({"enabled": "true"}),
            json!({"enabled": null}),
            json!({"enabled": true, "command": "anything"}),
        ] {
            let response = api.dispatch("powerSleep.setKeepAwake", params).await;
            assert_eq!(response["ok"], false);
            assert_eq!(response["error"]["code"], "validation-error");
        }
        for method in [
            "setDnd",
            "setAppPolicy",
            "delete",
            "dismiss",
            "snooze",
            "invokeAction",
            "reply",
        ] {
            let response = api
                .dispatch(&format!("notifications.{method}"), json!({}))
                .await;
            assert_eq!(response["error"]["code"], "validation-error", "{method}");
        }
        for method in [
            "queryCenter",
            "queryHistory",
            "list",
            "clear",
            "prepareDelete",
        ] {
            let response = api
                .dispatch(&format!("notifications.{method}"), json!({}))
                .await;
            assert_eq!(
                response["error"]["code"], "native-notifications-required",
                "{method}"
            );
        }
        for method in ["dismiss", "snooze", "invokeAction", "reply"] {
            let response = api
                .dispatch(
                    &format!("notifications.{method}"),
                    json!({"id":0, "until_unix_ms":1, "action_key":"default", "text":"reply"}),
                )
                .await;
            assert_eq!(
                response["error"]["message"],
                "notification id must be positive"
            );
        }
        assert_eq!(
            api.dispatch("notifications.unknown", json!({})).await["error"]["code"],
            "unsupported-method"
        );
    }

    #[tokio::test]
    async fn snapshot_projection_is_domain_local_and_validated() {
        let api = api().await;
        let full = api.dispatch("bar.snapshot", json!({})).await;
        for streams in [
            vec![],
            vec!["battery.changed", "battery.changed"],
            crate::protocol::STREAMS.to_vec(),
        ] {
            let projected = api
                .dispatch("bar.snapshot", json!({"streams": streams}))
                .await;
            let expected: serde_json::Map<String, serde_json::Value> = streams
                .iter()
                .map(|stream| {
                    let key = crate::protocol::read_wire::snapshot_key(stream);
                    (key.into(), full["data"]["snapshot"][key].clone())
                })
                .collect();
            assert_eq!(projected["data"]["snapshot"], json!(expected));
        }
        for invalid in [
            json!(["foreign"]),
            json!(null),
            json!("battery.changed"),
            json!([1]),
        ] {
            assert_eq!(
                api.dispatch("bar.snapshot", json!({"streams": invalid}))
                    .await["error"]["code"],
                "validation-error"
            );
        }
    }
}
