use std::sync::Arc;

pub(crate) use crate::protocol::{decode_request, error, success, unsupported_method};
use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
    media::MediaService,
    protocol,
    state::StateStore,
};
use serde_json::Value;

macro_rules! request {
    ($params:expr, $request:ty, $method:literal) => {
        match super::decode_request::<$request>($params, $method) {
            Ok(request) => request,
            Err(response) => return response,
        }
    };
}

mod activity;
mod audio;
mod battery;
mod effects;
mod notifications;
mod sleep;

use self::{
    activity::ActivityApi, battery::BatteryApi, effects::DesktopEffects,
    notifications::NotificationApi,
};

pub(crate) const BUS_NAME: &str = "org.laufan.BarDaemon";
pub(crate) const OBJECT_PATH: &str = "/org/laufan/BarDaemon";
pub(crate) const INTERFACE: &str = "org.laufan.BarDaemon2";

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
        let Some(streams) = streams.ok().and_then(|streams| {
            streams
                .into_iter()
                .map(protocol::read_wire::stream_spec)
                .collect::<Option<Vec<_>>>()
        }) else {
            return error(
                "validation-error",
                "streams must be a list of registered bar streams",
            );
        };
        self.state
            .read(|snapshot| {
                let projection: serde_json::Map<String, Value> = streams
                    .iter()
                    .map(|stream| (stream.snapshot_key.into(), stream.project(snapshot)))
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
            "locations.search" | "locations.add" | "locations.remove" => {
                self.activity.locations(method, params).await
            }
            "location.status" | "location.setManual" | "location.returnToAutomatic" => {
                match crate::timezone::location_control(method, params).await {
                    Ok(data) => success(data),
                    Err(failure) => error("location-error", failure.to_string()),
                }
            }
            "todos.create" => self.activity.todo_create(params).await,
            "todos.complete" => self.activity.todo_complete(params).await,
            "todos.delete" => self.activity.todo_delete(params).await,
            "workspace.focus" => self.effects.focus_workspace(params).await,
            "media.operation" => self.effects.media_operation(params).await,
            method if protocol::audio_wire::Method::parse(method).is_some() => {
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
            "updates.getOverview" => match crate::updates::overview::get(params).await {
                Ok(data) => success(data),
                Err(failure) => error("update-overview-error", failure.to_string()),
            },
            "updates.readLog" => match crate::updates::history::read_log(params).await {
                Ok(data) => success(data),
                Err(failure) => error("update-log-error", failure.to_string()),
            },
            "updates.queryHistory" => match crate::updates::history::query(params).await {
                Ok(data) => success(data),
                Err(failure) => error(
                    if failure.is::<crate::updates::history::StaleCursor>() {
                        "update-history-cursor-stale"
                    } else {
                        "update-history-error"
                    },
                    failure.to_string(),
                ),
            },
            _ => unsupported_method(method),
        };
        self.state.fence_reply(response)
    }
}
