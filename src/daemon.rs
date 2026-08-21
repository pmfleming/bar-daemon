use std::{env, sync::Arc};

use anyhow::{Context, Result};
use tokio::signal::{
    ctrl_c,
    unix::{SignalKind, signal},
};
use zbus::connection;

use crate::{
    activity::{ActivityService, notifications},
    api::{ApiService, BUS_NAME, OBJECT_PATH},
    state::StateStore,
};

use self::{subscriptions::BarDaemon, tasks::MonitorTasks};

mod subscriptions;
mod tasks;

pub async fn run() -> Result<()> {
    let state = StateStore::default();
    let native_notifications = !env::var("BAR_DAEMON_NOTIFICATION_BACKEND")
        .is_ok_and(|value| value.eq_ignore_ascii_case("swaync"));
    let notification_engine = if native_notifications {
        Some(
            notifications::engine::NotificationEngine::persistent(
                state.clone(),
                crate::activity::config::notification_database_path(),
            )
            .await
            .context("initialize native notification engine")?,
        )
    } else {
        None
    };
    let notification_service = match notification_engine.as_ref() {
        Some(engine) => notifications::service::NotificationService::native(Arc::clone(engine)),
        None => notifications::service::NotificationService::swaync(),
    };
    let activity = ActivityService::new(state.clone(), notification_service.sink()).await;
    let api = ApiService::new(
        state.clone(),
        Arc::clone(&activity),
        Arc::clone(&notification_service),
    );
    let daemon = BarDaemon::new(api, state.clone());
    let builder = connection::Builder::session()
        .context("connect to session D-Bus")?
        .name(BUS_NAME)
        .context("claim bar-daemon bus name")?
        .serve_at(OBJECT_PATH, daemon)
        .context("export bar-daemon interface")?;
    let builder = if let Some(engine) = notification_engine.as_ref() {
        builder
            .name(notifications::server::BUS_NAME)
            .context("claim notification server bus name")?
            .serve_at(
                notifications::server::OBJECT_PATH,
                notifications::server::NotificationServer::new(Arc::clone(engine)),
            )
            .context("export notification server interface")?
    } else {
        builder
    };
    let connection = builder
        .build()
        .await
        .context("start bar-daemon D-Bus service")?;
    let _tasks = MonitorTasks::spawn(
        state,
        activity,
        notification_service,
        notification_engine,
        connection,
    );

    tracing::info!(
        bus_name = BUS_NAME,
        object_path = OBJECT_PATH,
        "bar-daemon started"
    );
    let mut terminate = signal(SignalKind::terminate()).context("listen for SIGTERM")?;
    tokio::select! {
        result = ctrl_c() => result.context("wait for Ctrl-C"),
        _ = terminate.recv() => Ok(()),
    }
}
