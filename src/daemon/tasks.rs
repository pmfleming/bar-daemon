use futures::FutureExt;
use std::{future::Future, panic::AssertUnwindSafe, sync::Arc};

use shelllist_daemon_tokio::TaskGroup;

use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
    audio, battery,
    brightness::BrightnessService,
    hyprland,
    media::{self, MediaService},
    osd_hardware, power, sleep,
    state::StateStore,
    timezone, updates,
};

pub(super) struct MonitorTasks {
    tasks: Monitors,
}

struct Monitors {
    group: TaskGroup,
    state: StateStore,
}

impl Monitors {
    fn spawn(&self, name: &'static str, future: impl Future<Output = ()> + Send + 'static) {
        let state = self.state.clone();
        self.group.spawn(name, supervise(state, name, future));
    }
}

async fn supervise(state: StateStore, name: &'static str, future: impl Future<Output = ()>) {
    use crate::protocol::stream::*;
    let domains: &[&str] = match name {
        "activity" => &[ACTIVITY],
        "hyprland" => &[WORKSPACES],
        "work-area" => &[WORKAREA],
        "compositor-preferences" => &[COMPOSITOR],
        "media" => &[MEDIA],
        "audio" => &[AUDIO],
        "brightness" => &[BRIGHTNESS],
        "osd-hardware" => &[OSD_HARDWARE],
        "battery" => &[BATTERY],
        "power" => &[POWER_PROFILE],
        "sleep" => &[POWER_SLEEP],
        "sleep-policy" | "lid" | "critical-battery" => &[SLEEP_POLICY],
        "display-policy" => &[DISPLAY_POLICY],
        "updates" => &[UPDATES],
        "timezone" => &[TIMEZONE],
        "notifications" => &[NOTIFICATIONS, NOTIFICATION_ACTIVE],
        _ => unreachable!("monitor must declare its domains"),
    };
    tracing::info!(monitor = name, available = true, "bar monitor started");
    let reason = match AssertUnwindSafe(future).catch_unwind().await {
        Ok(()) => "exited",
        Err(_) => "panicked",
    };
    state.monitor_failed(name, domains, reason).await;
}

impl MonitorTasks {
    pub(super) fn spawn(
        state: StateStore,
        activity: Arc<ActivityService>,
        notifications: Arc<NotificationService>,
        media: MediaService,
        brightness: BrightnessService,
        connection: zbus::Connection,
    ) -> Self {
        let tasks = Monitors {
            group: TaskGroup::default(),
            state: state.clone(),
        };
        tasks.spawn("activity", activity.monitor());
        tasks.spawn("hyprland", hyprland::monitor(state.clone()));
        tasks.spawn("work-area", crate::work_area::monitor(state.clone()));
        tasks.spawn(
            "compositor-preferences",
            crate::compositor::monitor(state.clone()),
        );
        tasks.spawn("media", media::monitor(state.clone(), media));
        tasks.spawn("audio", audio::monitor(state.clone()));
        tasks.spawn("brightness", brightness.monitor());
        tasks.spawn("osd-hardware", osd_hardware::monitor(state.clone()));
        tasks.spawn(
            "battery",
            battery::monitor(state.clone(), notifications.sink()),
        );
        tasks.spawn("power", power::monitor(state.clone()));
        tasks.spawn("sleep", sleep::monitor(state.clone()));
        tasks.spawn("sleep-policy", crate::sleep_policy::monitor(state.clone()));
        tasks.spawn("lid", crate::sleep_policy::lid::monitor(state.clone()));
        tasks.spawn(
            "critical-battery",
            crate::sleep_policy::critical::monitor(state.clone(), notifications.sink()),
        );
        tasks.spawn(
            "display-policy",
            crate::display_policy::monitor(state.clone()),
        );
        tasks.spawn("updates", updates::monitor(state.clone()));
        tasks.spawn("timezone", timezone::monitor(state.clone()));
        tasks.spawn("notifications", notifications.monitor(state, connection));
        Self { tasks }
    }

    pub(super) async fn shutdown(&self) {
        self.tasks.group.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::AudioState, protocol::stream};

    #[tokio::test]
    async fn panic_and_early_exit_are_sticky_unavailable_without_killing_other_domains() {
        for panic in [false, true] {
            let state = StateStore::default();
            state
                .update_audio(AudioState {
                    available: true,
                    ..Default::default()
                })
                .await;
            let mut audio = state.subscribe_domains(&[stream::AUDIO]);
            let mut battery = state.subscribe_domains(&[stream::BATTERY]);
            supervise(state.clone(), "audio", async move {
                assert!(!panic, "injected monitor panic");
            })
            .await;
            let unavailable = audio.recv().await.unwrap();
            assert_eq!(unavailable.data["available"], false);
            assert!(
                unavailable.data["error"]
                    .as_str()
                    .unwrap()
                    .contains(if panic { "panicked" } else { "exited" })
            );
            assert!(battery.try_recv().is_err(), "unrelated domain is untouched");
            assert!(!state.snapshot().await.audio.available);
            assert!(!state.read(|s| s.audio.available).await);
            let reply = state.fence_reply(crate::api::success(serde_json::json!({
                "audio": {"available": true}, "operation": {"completed": true}
            })));
            assert_eq!(reply["data"]["audio"]["available"], false);
            assert_eq!(reply["data"]["operation"]["completed"], true);
            // A late action completion must not claim the dead monitor recovered.
            state
                .update_audio(AudioState {
                    available: true,
                    volume_percent: 42,
                    ..Default::default()
                })
                .await;
            assert_eq!(audio.recv().await.unwrap().data["available"], false);
            let (snapshot, _) = state.snapshot_and_subscribe_domains(&[stream::AUDIO]).await;
            assert!(!snapshot.audio.available);
        }
    }
}
