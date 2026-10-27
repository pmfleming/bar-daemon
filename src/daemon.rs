use std::{env, sync::Arc};

use anyhow::{Context, Result};
use zbus::connection;

use crate::{
    activity::{ActivityService, notifications},
    api::{ApiService, BUS_NAME, OBJECT_PATH},
    media::MediaService,
    state::StateStore,
};

use self::{subscriptions::BarDaemon, tasks::MonitorTasks};

#[cfg(test)]
mod agenda_tests;
pub(crate) mod battery_operations;
mod owner;
mod socket;
mod subscriptions;
mod tasks;

async fn claim_names(connection: &zbus::Connection, native_notifications: bool) -> Result<()> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    for name in std::iter::once(BUS_NAME)
        .chain(native_notifications.then_some(notifications::server::BUS_NAME))
    {
        let reply = connection
            .request_name_with_flags(name, RequestNameFlags::DoNotQueue.into())
            .await
            .with_context(|| {
                format!("claim exclusive {name} before initializing persistent stores")
            })?;
        anyhow::ensure!(
            reply == RequestNameReply::PrimaryOwner,
            "another daemon owns {name}; stop it before agenda migration"
        );
    }
    Ok(())
}

pub(crate) async fn run() -> Result<()> {
    let state = StateStore::default();
    let native_notifications = !env::var("BAR_DAEMON_NOTIFICATION_BACKEND")
        .is_ok_and(|value| value.eq_ignore_ascii_case("swaync"));
    // Old JSON-writing versions do not know the new DB lock. Claim the same
    // exclusive bus owner first, so a second daemon cannot race migration.
    let connection = connection::Builder::session()
        .context("connect to session D-Bus")?
        .build()
        .await
        .context("start bar-daemon D-Bus connection")?;
    claim_names(&connection, native_notifications).await?;
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
    let activity = ActivityService::new(state.clone()).await;
    let media = MediaService::default();
    let brightness = crate::brightness::BrightnessService::new(state.clone());
    let api = ApiService::new(
        state.clone(),
        Arc::clone(&activity),
        Arc::clone(&notification_service),
        media.clone(),
        brightness.clone(),
    );
    let daemon = BarDaemon::new(api, state.clone());
    let subscriptions = Arc::clone(&daemon.subscriptions);
    connection
        .object_server()
        .at(OBJECT_PATH, daemon)
        .await
        .context("export bar-daemon interface")?;
    if let Some(engine) = notification_engine.as_ref() {
        connection
            .object_server()
            .at(
                notifications::server::OBJECT_PATH,
                notifications::server::NotificationServer::new(Arc::clone(engine)),
            )
            .await
            .context("export notification server interface")?;
    }
    let tasks = MonitorTasks::spawn(
        state,
        Arc::clone(&activity),
        notification_service,
        media,
        brightness,
        connection.clone(),
    );

    tracing::info!(
        bus_name = BUS_NAME,
        object_path = OBJECT_PATH,
        "bar-daemon started"
    );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut listener = tokio::spawn(socket::listen(connection, async {
        let _ = stopped.await;
    }));
    let (mut result, joined) = tokio::select! {
        result = shelllist_daemon_tokio::wait_for_shutdown() => (result, false),
        result = &mut listener => (result.context("join bar socket listener").and_then(|r| r), true),
    };
    let _ = stop.send(());
    if !joined {
        result = result.and(
            listener
                .await
                .context("join bar socket shutdown")
                .and_then(|r| r),
        );
    }
    subscriptions.shutdown().await;
    tasks.shutdown().await;
    result.and(
        activity
            .shutdown()
            .await
            .context("drain agenda store on shutdown"),
    )
}
