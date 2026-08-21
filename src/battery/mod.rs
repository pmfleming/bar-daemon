//! ThinkPad-focused battery monitoring, policy, and control.
//!
//! The public module boundary stays stable while providers and policy are kept
//! isolated so the UPower compatibility backend can be removed independently.

use std::{env, path::PathBuf, time::Duration};

use anyhow::{Result, bail};
use futures::StreamExt;
use tokio::time::{interval, sleep};
use zvariant::OwnedObjectPath;

use crate::{
    activity::notifications::service::NotificationSink, model::BatteryState, state::StateStore,
};

mod model;
mod monitor;
mod policy;
mod sysfs;
mod upower;

use policy::{AlertTracker, send_notification};

pub(crate) const WARNING_PERCENT: u8 = 25;
pub(crate) const CRITICAL_PERCENT: u8 = 12;

pub async fn monitor(store: StateStore, notifications: NotificationSink) {
    if env::var("BAR_DAEMON_BATTERY_BACKEND").as_deref() == Ok("upower") {
        monitor_upower_forever(store, notifications).await;
    } else {
        monitor_native(store, notifications).await;
    }
}

async fn monitor_native(store: StateStore, notifications: NotificationSink) {
    let mut alerts = AlertTracker::default();
    let mut native = monitor::NativeMonitor::new(PathBuf::from("/sys/class/power_supply"));
    loop {
        let power_supply = native.power_supply().clone();
        match tokio::task::spawn_blocking(move || power_supply.read_state()).await {
            Ok(Ok(state)) => publish(state, &store, &mut alerts, &notifications).await,
            Ok(Err(error)) => publish_error(error, &store).await,
            Err(error) => publish_error(error, &store).await,
        }
        native.changed().await;
    }
}

async fn monitor_upower_forever(store: StateStore, notifications: NotificationSink) {
    let mut alerts = AlertTracker::default();
    loop {
        match zbus::Connection::system().await {
            Ok(connection) => {
                if let Err(error) =
                    monitor_upower(&connection, &store, &mut alerts, &notifications).await
                {
                    tracing::warn!(%error, "UPower monitor disconnected; using sysfs fallback");
                    refresh_native_once(&store, &mut alerts, &notifications).await;
                }
            }
            Err(error) => {
                store
                    .update_battery(BatteryState {
                        error: Some(error.to_string()),
                        ..BatteryState::default()
                    })
                    .await
            }
        }
        sleep(Duration::from_secs(5)).await;
    }
}

async fn monitor_upower(
    connection: &zbus::Connection,
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) -> Result<()> {
    let device_path = upower::display_device_path(connection).await?;
    let root_properties = zbus::Proxy::new(
        connection,
        upower::BUS,
        upower::ROOT_PATH,
        upower::PROPERTIES_INTERFACE,
    )
    .await?;
    let device_properties = zbus::Proxy::new(
        connection,
        upower::BUS,
        device_path.as_str(),
        upower::PROPERTIES_INTERFACE,
    )
    .await?;
    let mut root_changes = root_properties.receive_signal("PropertiesChanged").await?;
    let mut device_changes = device_properties
        .receive_signal("PropertiesChanged")
        .await?;
    let mut fallback = interval(Duration::from_secs(30));
    fallback.tick().await;
    refresh_upower(connection, store, alerts, &device_path, notifications).await;
    loop {
        tokio::select! {
            signal = root_changes.next() => {
                if signal.is_none() { bail!("UPower root property stream ended"); }
                refresh_upower(connection, store, alerts, &device_path, notifications).await;
            }
            signal = device_changes.next() => {
                if signal.is_none() { bail!("UPower device property stream ended"); }
                refresh_upower(connection, store, alerts, &device_path, notifications).await;
            }
            _ = fallback.tick() => refresh_upower(connection, store, alerts, &device_path, notifications).await,
        }
    }
}

async fn refresh_upower(
    connection: &zbus::Connection,
    store: &StateStore,
    alerts: &mut AlertTracker,
    path: &OwnedObjectPath,
    notifications: &NotificationSink,
) {
    match upower::read_state(connection, path).await {
        Ok(state) => publish(state, store, alerts, notifications).await,
        Err(error) => publish_error(error, store).await,
    }
}

async fn refresh_native_once(
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) {
    match tokio::task::spawn_blocking(|| sysfs::PowerSupplyFs::system().read_state()).await {
        Ok(Ok(state)) => publish(state, store, alerts, notifications).await,
        Ok(Err(error)) => publish_error(error, store).await,
        Err(error) => publish_error(error, store).await,
    }
}

async fn publish(
    state: BatteryState,
    store: &StateStore,
    alerts: &mut AlertTracker,
    notifications: &NotificationSink,
) {
    if let Some(alert) = alerts.observe(&state) {
        tokio::spawn(send_notification(alert, notifications.clone()));
    }
    store.update_battery(state).await;
}

async fn publish_error(error: impl std::fmt::Display, store: &StateStore) {
    store
        .update_battery(BatteryState {
            error: Some(error.to_string()),
            ..BatteryState::default()
        })
        .await
}
