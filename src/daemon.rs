use std::{env, sync::Arc};

use anyhow::{Context, Result};
use zbus::connection;

use crate::{
    activity::{ActivityService, notifications},
    api::{ApiService, OBJECT_PATH},
    media::MediaService,
    state::StateStore,
};

use self::{subscriptions::BarDaemon, tasks::MonitorTasks};

#[cfg(test)]
mod agenda_tests;
pub(crate) mod battery_operations;
mod owner;
mod search;
mod socket;
mod subscriptions;
mod tasks;

async fn claim_names(
    connection: &zbus::Connection,
    component: crate::component::Component,
    native_notifications: bool,
) -> Result<()> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    for name in std::iter::once(component.bus_name())
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
    run_component(crate::component::Component::Bar).await
}

pub(crate) async fn run_component(component: crate::component::Component) -> Result<()> {
    let state = StateStore::for_component(component);
    let native_notifications = component == crate::component::Component::Notifications
        && !env::var("BAR_DAEMON_NOTIFICATION_BACKEND")
            .is_ok_and(|value| value.eq_ignore_ascii_case("swaync"));
    // Old JSON-writing versions do not know the new DB lock. Claim the same
    // exclusive bus owner first, so a second daemon cannot race migration.
    let connection = connection::Builder::session()
        .context("connect to session D-Bus")?
        .build()
        .await
        .context("start bar-daemon D-Bus connection")?;
    claim_names(&connection, component, native_notifications).await?;
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
        None if component == crate::component::Component::Notifications => {
            notifications::service::NotificationService::swaync()
        }
        None => notifications::service::NotificationService::remote(),
    };
    let activity = if component.owns_stream(crate::protocol::stream::ACTIVITY) {
        Some(ActivityService::new(state.clone()).await)
    } else {
        None
    };
    let media = MediaService::default();
    let brightness = crate::brightness::BrightnessService::new(state.clone());
    let api = ApiService::new(
        state.clone(),
        activity.clone(),
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
    if let Some(engine) = notification_engine.as_ref() {
        connection
            .object_server()
            .at(
                OBJECT_PATH,
                notifications::service::InternalNotifications(Arc::clone(engine)),
            )
            .await?;
    }
    let tasks = MonitorTasks::spawn(
        state.clone(),
        activity.clone(),
        notification_service,
        media,
        brightness,
        connection.clone(),
    );

    tracing::info!(
        bus_name = component.bus_name(),
        object_path = OBJECT_PATH,
        "bar-daemon started"
    );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut listener = tokio::spawn(socket::listen(connection, component, async {
        let _ = stopped.await;
    }));
    let (mut result, joined) = tokio::select! {
        result = shelllist_daemon_tokio::wait_for_shutdown() => (result, false),
        error = state.compositor_failure(), if component.owns_stream(crate::protocol::stream::WORKSPACES) => (Err(error), false),
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
    if let Some(engine) = notification_engine {
        result = result.and(
            engine
                .shutdown()
                .await
                .context("drain notification store on shutdown"),
        );
    }
    if let Some(activity) = activity {
        result = result.and(
            activity
                .shutdown()
                .await
                .context("drain agenda store on shutdown"),
        );
    }
    result
}
