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
    if component == crate::component::Component::Activity {
        reject_legacy_activity_owner(connection).await?;
    }

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
            "another daemon owns {name}; stop it before starting {}",
            component.name()
        );
    }
    Ok(())
}

async fn reserve_startup(
    connection: &zbus::Connection,
    component: crate::component::Component,
    native_notifications: bool,
) -> Result<()> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let reservation = format!("{}.Starting", component.bus_name());
    let reply = connection
        .request_name_with_flags(reservation.as_str(), RequestNameFlags::DoNotQueue.into())
        .await?;
    anyhow::ensure!(
        reply == RequestNameReply::PrimaryOwner,
        "{} startup is already in progress",
        component.name()
    );
    if component == crate::component::Component::Activity {
        reject_legacy_activity_owner(connection).await?;
    }
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    for name in std::iter::once(component.bus_name())
        .chain(native_notifications.then_some(notifications::server::BUS_NAME))
    {
        anyhow::ensure!(
            !bus.name_has_owner(name.try_into()?).await?,
            "another daemon owns {name}"
        );
    }
    Ok(())
}

async fn reject_legacy_activity_owner(connection: &zbus::Connection) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let bus = zbus::fdo::DBusProxy::new(connection).await?;
        if !bus.name_has_owner(crate::component::Component::Bar.bus_name().try_into()?).await? { return Ok(()); }
        let proxy = zbus::Proxy::new(connection, crate::component::Component::Bar.bus_name(), OBJECT_PATH, crate::api::INTERFACE).await?;
        let owner = proxy.call::<_, _, String>("Component", &()).await;
        anyhow::ensure!(owner.as_deref().ok() == Some("bar-daemon"),
            "legacy bar-daemon may own the agenda store; stop it before starting bar-activity-daemon");
        Ok(())
    }).await.context("identify legacy agenda owner before migration")?
}

pub(crate) async fn run() -> Result<()> {
    run_component(crate::component::Component::Bar).await
}

pub(crate) async fn run_component(component: crate::component::Component) -> Result<()> {
    let state = StateStore::for_component(component);
    let native_notifications = component == crate::component::Component::Notifications
        && !env::var("BAR_DAEMON_NOTIFICATION_BACKEND")
            .is_ok_and(|value| value.eq_ignore_ascii_case("swaync"));
    // Claim component ownership before opening persistent stores. Activity also
    // checks for a legacy monolith that may not know the agenda ownership lock.
    let connection = connection::Builder::session()
        .context("connect to session D-Bus")?
        .build()
        .await
        .context("start bar-daemon D-Bus connection")?;
    // A private reservation serializes store startup without advertising readiness.
    reserve_startup(&connection, component, native_notifications).await?;
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
    if component == crate::component::Component::Notifications {
        connection
            .object_server()
            .at(
                OBJECT_PATH,
                notifications::service::InternalNotifications(Arc::clone(&notification_service)),
            )
            .await?;
    }
    // Public names are the readiness boundary: stores and every interface exist.
    if let Err(error) = claim_names(&connection, component, native_notifications).await {
        if let Some(engine) = &notification_engine {
            let _ = engine.shutdown().await;
        }
        if let Some(activity) = &activity {
            let _ = activity.shutdown().await;
        }
        return Err(error);
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
        error = tasks.failure() => (Err(error), false),
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
