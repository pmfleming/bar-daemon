use std::sync::Arc;

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
    tasks: TaskGroup,
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
        let tasks = TaskGroup::default();
        tasks.spawn("activity", activity.monitor());
        tasks.spawn("hyprland", hyprland::monitor(state.clone()));
        tasks.spawn("work-area", crate::work_area::monitor(state.clone()));
        tasks.spawn("compositor-preferences", crate::compositor::monitor(state.clone()));
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
        self.tasks.shutdown().await;
    }
}
