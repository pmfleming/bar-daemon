use std::sync::Arc;

use shelllist_daemon_tokio::TaskGroup;

use crate::{
    activity::{
        ActivityService,
        notifications::{
            engine::NotificationEngine, server::forward_signals, service::NotificationService,
        },
    },
    audio, battery,
    brightness::BrightnessService,
    hyprland,
    media::{self, MediaService},
    osd_hardware, power, sleep,
    state::StateStore,
    timezone, updates,
};

pub(super) struct MonitorTasks {
    tasks: TaskGroup,
}

impl MonitorTasks {
    pub(super) fn spawn(
        state: StateStore,
        activity: Arc<ActivityService>,
        notifications: Arc<NotificationService>,
        notification_engine: Option<Arc<NotificationEngine>>,
        media: MediaService,
        brightness: BrightnessService,
        connection: zbus::Connection,
    ) -> Self {
        let tasks = TaskGroup::default();
        tasks.spawn("activity", activity.monitor());
        tasks.spawn("hyprland", hyprland::monitor(state.clone()));
        tasks.spawn("work-area", crate::work_area::monitor(state.clone()));
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
        tasks.spawn("updates", updates::monitor(state.clone()));
        tasks.spawn("timezone", timezone::monitor(state.clone()));
        if let Some(engine) = notification_engine {
            tasks.spawn("notification-expiry", Arc::clone(&engine).run_expiry());
            tasks.spawn("notification-signals", forward_signals(engine, connection));
        } else {
            tasks.spawn(
                "notifications",
                crate::activity::notifications::monitor(state),
            );
        }
        Self { tasks }
    }

    pub(super) async fn shutdown(&self) {
        self.tasks.shutdown().await;
    }
}
