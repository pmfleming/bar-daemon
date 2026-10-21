use futures::FutureExt;
use std::{future::Future, panic::AssertUnwindSafe, sync::Arc};

use shelllist_daemon_tokio::TaskGroup;

use crate::{
    activity::{ActivityService, notifications::service::NotificationService},
    audio, battery,
    brightness::BrightnessService,
    hyprland,
    media::{self, MediaService},
    osd_hardware, power,
    protocol::stream::{
        ACTIVITY, AUDIO, BATTERY, BRIGHTNESS, COMPOSITOR, DISPLAY_POLICY, MEDIA,
        NOTIFICATION_ACTIVE, NOTIFICATIONS, OSD_HARDWARE, POWER_PROFILE, POWER_SLEEP, SLEEP_POLICY,
        TIMEZONE, UPDATES, WORKAREA, WORKSPACES,
    },
    sleep,
    state::StateStore,
    timezone, updates,
};

pub(super) struct MonitorTasks {
    group: TaskGroup,
    state: StateStore,
}

async fn supervise(
    state: StateStore,
    name: &'static str,
    domains: &'static [&'static str],
    future: impl Future<Output = ()>,
) {
    tracing::info!(monitor = name, available = true, "bar monitor started");
    let reason = match AssertUnwindSafe(future).catch_unwind().await {
        Ok(()) => "exited",
        Err(_) => "panicked",
    };
    state.monitor_failed(name, domains, reason).await;
}

impl MonitorTasks {
    fn add(
        &self,
        name: &'static str,
        domains: &'static [&'static str],
        future: impl Future<Output = ()> + Send + 'static,
    ) {
        self.group
            .spawn(name, supervise(self.state.clone(), name, domains, future));
    }

    pub(super) fn spawn(
        state: StateStore,
        activity: Arc<ActivityService>,
        notifications: Arc<NotificationService>,
        media: MediaService,
        brightness: BrightnessService,
        connection: zbus::Connection,
    ) -> Self {
        let tasks = Self {
            group: TaskGroup::default(),
            state: state.clone(),
        };
        tasks.add("activity", &[ACTIVITY], activity.monitor());
        tasks.add("hyprland", &[WORKSPACES], hyprland::monitor(state.clone()));
        tasks.add(
            "work-area",
            &[WORKAREA],
            crate::work_area::monitor(state.clone()),
        );
        tasks.add(
            "compositor-preferences",
            &[COMPOSITOR],
            crate::compositor::monitor(state.clone()),
        );
        tasks.add("media", &[MEDIA], media::monitor(state.clone(), media));
        tasks.add("audio", &[AUDIO], audio::monitor(state.clone()));
        tasks.add("brightness", &[BRIGHTNESS], brightness.monitor());
        tasks.add(
            "osd-hardware",
            &[OSD_HARDWARE],
            osd_hardware::monitor(state.clone()),
        );
        tasks.add(
            "battery",
            &[BATTERY],
            battery::monitor(state.clone(), notifications.sink()),
        );
        tasks.add("power", &[POWER_PROFILE], power::monitor(state.clone()));
        tasks.add("sleep", &[POWER_SLEEP], sleep::monitor(state.clone()));
        tasks.add(
            "sleep-policy",
            &[SLEEP_POLICY],
            crate::sleep_policy::monitor(state.clone()),
        );
        tasks.add(
            "lid",
            &[SLEEP_POLICY],
            crate::sleep_policy::lid::monitor(state.clone()),
        );
        tasks.add(
            "critical-battery",
            &[SLEEP_POLICY],
            crate::sleep_policy::critical::monitor(state.clone(), notifications.sink()),
        );
        tasks.add(
            "display-policy",
            &[DISPLAY_POLICY],
            crate::display_policy::monitor(state.clone()),
        );
        tasks.add(
            "updates",
            &[UPDATES],
            updates::monitor(state.clone(), notifications.clone()),
        );
        tasks.add("timezone", &[TIMEZONE], timezone::monitor(state.clone()));
        tasks.add(
            "notifications",
            &[NOTIFICATIONS, NOTIFICATION_ACTIVE],
            notifications.monitor(state, connection),
        );
        tasks
    }

    pub(super) async fn shutdown(&self) {
        self.group.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::supervise;
    use crate::{model::AudioState, protocol::stream, state::StateStore};

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
            supervise(state.clone(), "audio", &[stream::AUDIO], async move {
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
