use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::{
    activity::{
        ActivityService,
        notifications::{
            engine::NotificationEngine, server::forward_signals, service::NotificationService,
        },
    },
    audio, battery, brightness, hyprland, media, power,
    state::StateStore,
    timezone, updates,
};

pub(super) struct MonitorTasks {
    tasks: Vec<JoinHandle<()>>,
}

impl MonitorTasks {
    pub(super) fn spawn(
        state: StateStore,
        activity: Arc<ActivityService>,
        notifications: Arc<NotificationService>,
        notification_engine: Option<Arc<NotificationEngine>>,
        connection: zbus::Connection,
    ) -> Self {
        let mut tasks = vec![
            tokio::spawn(activity.monitor()),
            tokio::spawn(hyprland::monitor(state.clone())),
            tokio::spawn(media::monitor(state.clone())),
            tokio::spawn(audio::monitor(state.clone())),
            tokio::spawn(brightness::monitor(state.clone())),
            tokio::spawn(battery::monitor(state.clone(), notifications.sink())),
            tokio::spawn(power::monitor(state.clone())),
            tokio::spawn(updates::monitor(state.clone())),
            tokio::spawn(timezone::monitor(state.clone())),
        ];
        if let Some(engine) = notification_engine {
            tasks.push(tokio::spawn(Arc::clone(&engine).run_expiry()));
            tasks.push(tokio::spawn(forward_signals(engine, connection)));
        } else {
            tasks.push(tokio::spawn(crate::activity::notifications::monitor(state)));
        }
        Self { tasks }
    }
}

impl Drop for MonitorTasks {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
