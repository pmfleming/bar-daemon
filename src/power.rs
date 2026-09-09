use std::{collections::HashMap, sync::OnceLock, time::Duration};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::{
    sync::Mutex,
    time::{interval, sleep},
};
use zvariant::OwnedValue;

use crate::{
    model::{BatteryState, PowerProfile, PowerProfileAction, PowerProfileHold, PowerProfileState},
    protocol,
    state::StateStore,
};

const BUS: &str = "org.freedesktop.UPower.PowerProfiles";
const PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
const INTERFACE: &str = "org.freedesktop.UPower.PowerProfiles";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const HOLD_APPLICATION_ID: &str = "org.laufan.BarDaemon";
mod automation;
#[cfg(test)]
mod automation_tests;
use automation::PowerEnvelope;

static POWER_ENVELOPE: OnceLock<Mutex<PowerEnvelope>> = OnceLock::new();

fn power_envelope() -> &'static Mutex<PowerEnvelope> {
    POWER_ENVELOPE.get_or_init(|| Mutex::new(PowerEnvelope::default()))
}

pub(crate) async fn monitor(store: StateStore) {
    loop {
        match zbus::Connection::system().await {
            Ok(connection) => {
                if let Err(error) = monitor_connection(&connection, &store).await {
                    store
                        .update_power_profile(PowerProfileState {
                            error: Some(error.to_string()),
                            ..PowerProfileState::default()
                        })
                        .await;
                }
            }
            Err(error) => {
                store
                    .update_power_profile(PowerProfileState {
                        error: Some(error.to_string()),
                        ..PowerProfileState::default()
                    })
                    .await
            }
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn monitor_connection(connection: &zbus::Connection, store: &StateStore) -> Result<()> {
    power_envelope()
        .lock()
        .await
        .attach(connection.clone())
        .await?;
    let result = monitor_attached_connection(connection, store).await;
    power_envelope().lock().await.detach();
    result
}

async fn monitor_attached_connection(
    connection: &zbus::Connection,
    store: &StateStore,
) -> Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    let properties = zbus::Proxy::new(connection, BUS, PATH, PROPERTIES_INTERFACE).await?;
    let mut changes = properties.receive_signal("PropertiesChanged").await?;
    let mut events = store.subscribe();
    let mut fallback = interval(Duration::from_secs(60));
    fallback.tick().await;
    reconcile_battery_profile(&store.snapshot().await.battery).await;
    refresh(connection, store).await;
    loop {
        tokio::select! {
            signal = owner_changes.next() => {
                let Some(signal) = signal else { bail!("D-Bus owner-change stream ended"); };
                if signal.args()?.name().as_str() == BUS {
                    bail!("Power Profiles service owner changed");
                }
            }
            signal = changes.next() => {
                if signal.is_none() { bail!("power-profiles-daemon property stream ended"); }
                reconcile_battery_profile(&store.snapshot().await.battery).await;
                refresh(connection, store).await;
            }
            event = events.recv() => {
                match event {
                    Ok(event) if event.stream == protocol::stream::BATTERY => {
                        reconcile_battery_profile(&store.snapshot().await.battery).await;
                        refresh(connection, store).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        reconcile_battery_profile(&store.snapshot().await.battery).await;
                        refresh(connection, store).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        bail!("state event stream ended");
                    }
                }
            }
            _ = fallback.tick() => {
                reconcile_battery_profile(&store.snapshot().await.battery).await;
                refresh(connection, store).await;
            },
        }
    }
}

async fn reconcile_battery_profile(battery: &BatteryState) {
    let mut envelope = power_envelope().lock().await;
    if let Err(error) = envelope.reconcile(battery).await {
        tracing::warn!(%error, "battery profile automation failed");
        envelope.status.status = "error".into();
        envelope.status.error = Some(error.to_string());
    }
}

async fn refresh(connection: &zbus::Connection, store: &StateStore) {
    match read_state(connection).await {
        Ok(state) => store.update_power_profile(state).await,
        Err(error) => {
            store
                .update_power_profile(PowerProfileState {
                    error: Some(error.to_string()),
                    ..PowerProfileState::default()
                })
                .await
        }
    }
}

async fn read_state(connection: &zbus::Connection) -> Result<PowerProfileState> {
    let mut state = read_raw_state(connection).await?;
    state.battery_automation = power_envelope().lock().await.status.clone();
    Ok(state)
}

async fn read_raw_state(connection: &zbus::Connection) -> Result<PowerProfileState> {
    let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE)
        .await
        .context("connect to power-profiles-daemon")?;
    let profile: String = proxy
        .get_property("ActiveProfile")
        .await
        .context("read active power profile")?;
    let raw_profiles: Vec<HashMap<String, OwnedValue>> =
        proxy.get_property("Profiles").await.unwrap_or_default();
    let profiles = raw_profiles
        .iter()
        .filter_map(parse_profile)
        .collect::<Vec<_>>();
    let driver = profiles
        .iter()
        .find(|item| item.name == profile)
        .map(|item| item.driver.clone())
        .unwrap_or_default();
    let raw_action_info: Vec<HashMap<String, OwnedValue>> =
        proxy.get_property("ActionsInfo").await.unwrap_or_default();
    let mut actions = raw_action_info
        .iter()
        .filter_map(parse_action)
        .collect::<Vec<_>>();
    if actions.is_empty() {
        actions = proxy
            .get_property::<Vec<String>>("Actions")
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|name| PowerProfileAction {
                name,
                description: String::new(),
                enabled: true,
            })
            .collect();
    }
    let active_holds = proxy
        .get_property::<Vec<HashMap<String, OwnedValue>>>("ActiveProfileHolds")
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(parse_hold)
        .collect();
    Ok(PowerProfileState {
        available: true,
        profile,
        driver,
        profiles,
        performance_degraded: proxy
            .get_property("PerformanceDegraded")
            .await
            .unwrap_or_default(),
        version: proxy.get_property("Version").await.unwrap_or_default(),
        battery_aware: proxy.get_property("BatteryAware").await.ok(),
        actions,
        active_holds,
        battery_automation: Default::default(),
        error: None,
    })
}

fn parse_profile(values: &HashMap<String, OwnedValue>) -> Option<PowerProfile> {
    let name = property_string(values, "Profile")?;
    Some(PowerProfile {
        name,
        driver: property_string(values, "Driver")
            .or_else(|| property_string(values, "CpuDriver"))
            .unwrap_or_default(),
        platform_driver: property_string(values, "PlatformDriver").unwrap_or_default(),
    })
}

fn parse_action(values: &HashMap<String, OwnedValue>) -> Option<PowerProfileAction> {
    Some(PowerProfileAction {
        name: property_string(values, "Name").or_else(|| property_string(values, "Action"))?,
        description: property_string(values, "Description").unwrap_or_default(),
        enabled: property_bool(values, "Enabled").unwrap_or(true),
    })
}

fn parse_hold(values: &HashMap<String, OwnedValue>) -> Option<PowerProfileHold> {
    Some(PowerProfileHold {
        application_id: property_string(values, "ApplicationId")?,
        profile: property_string(values, "Profile")?,
        reason: property_string(values, "Reason").unwrap_or_default(),
    })
}

fn property_string(values: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    values
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .map(str::to_string)
}

fn property_bool(values: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    values.get(key).and_then(|value| bool::try_from(value).ok())
}

pub(crate) async fn set_profile(
    profile: &str,
    battery: &BatteryState,
) -> Result<PowerProfileState> {
    if !matches!(profile, "power-saver" | "balanced" | "performance") {
        bail!("unsupported power profile: {profile}");
    }
    let connection = zbus::Connection::system()
        .await
        .context("connect to system D-Bus")?;
    let current = read_state(&connection).await?;
    if !current.profiles.iter().any(|item| item.name == profile) {
        bail!("power profile is unavailable: {profile}");
    }
    power_envelope()
        .lock()
        .await
        .select_manual(profile, battery, &connection)
        .await?;
    read_state(&connection).await
}

pub(crate) async fn resume_automatic(battery: &BatteryState) -> Result<PowerProfileState> {
    power_envelope().lock().await.resume(battery).await?;
    let connection = zbus::Connection::system().await?;
    read_state(&connection).await
}

pub(crate) async fn set_battery_aware(enabled: bool) -> Result<PowerProfileState> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to system D-Bus")?;
    let proxy = zbus::Proxy::new(&connection, BUS, PATH, INTERFACE)
        .await
        .context("connect to Power Profiles service")?;
    let current = read_state(&connection).await?;
    if current.battery_aware.is_none() {
        bail!("Power Profiles service does not expose battery-aware control");
    }
    proxy
        .set_property("BatteryAware", &enabled)
        .await
        .context("set battery-aware power profile behavior")?;
    read_state(&connection).await
}

pub(crate) async fn set_action_enabled(action: &str, enabled: bool) -> Result<PowerProfileState> {
    let connection = zbus::Connection::system()
        .await
        .context("connect to system D-Bus")?;
    let proxy = zbus::Proxy::new(&connection, BUS, PATH, INTERFACE)
        .await
        .context("connect to Power Profiles service")?;
    let current = read_state(&connection).await?;
    if !current.actions.iter().any(|item| item.name == action) {
        bail!("power profile action is unavailable: {action}");
    }
    proxy
        .call_method("SetActionEnabled", &(action, enabled))
        .await
        .context("configure power profile action")?;
    read_state(&connection).await
}
