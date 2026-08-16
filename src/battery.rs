use std::{collections::HashMap, path::Path, time::Duration};

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::time::{interval, sleep};
use zvariant::{OwnedObjectPath, OwnedValue};

use crate::{model::BatteryState, state::StateStore};

const BUS: &str = "org.freedesktop.UPower";
const ROOT_PATH: &str = "/org/freedesktop/UPower";
const ROOT_INTERFACE: &str = "org.freedesktop.UPower";
const DEVICE_INTERFACE: &str = "org.freedesktop.UPower.Device";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const WARNING_PERCENT: u8 = 25;
const CRITICAL_PERCENT: u8 = 12;

pub async fn monitor(store: StateStore) {
    let mut alerts = AlertTracker::default();
    loop {
        match zbus::Connection::system().await {
            Ok(connection) => {
                if let Err(error) = monitor_connection(&connection, &store, &mut alerts).await {
                    tracing::warn!(%error, "UPower monitor disconnected");
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
        sleep(Duration::from_secs(2)).await;
    }
}

async fn monitor_connection(
    connection: &zbus::Connection,
    store: &StateStore,
    alerts: &mut AlertTracker,
) -> Result<()> {
    let device_path = display_device_path(connection).await?;
    let root_properties =
        zbus::Proxy::new(connection, BUS, ROOT_PATH, PROPERTIES_INTERFACE).await?;
    let device_properties =
        zbus::Proxy::new(connection, BUS, device_path.as_str(), PROPERTIES_INTERFACE).await?;
    let mut root_changes = root_properties.receive_signal("PropertiesChanged").await?;
    let mut device_changes = device_properties
        .receive_signal("PropertiesChanged")
        .await?;
    let mut fallback = interval(Duration::from_secs(30));
    fallback.tick().await;
    refresh(connection, store, alerts, &device_path).await;
    loop {
        tokio::select! {
            signal = root_changes.next() => {
                if signal.is_none() { anyhow::bail!("UPower root property stream ended"); }
                refresh(connection, store, alerts, &device_path).await;
            }
            signal = device_changes.next() => {
                if signal.is_none() { anyhow::bail!("UPower device property stream ended"); }
                refresh(connection, store, alerts, &device_path).await;
            }
            _ = fallback.tick() => refresh(connection, store, alerts, &device_path).await,
        }
    }
}

async fn refresh(
    connection: &zbus::Connection,
    store: &StateStore,
    alerts: &mut AlertTracker,
    path: &OwnedObjectPath,
) {
    match read_state(connection, path).await {
        Ok(state) => {
            if let Some(alert) = alerts.observe(&state) {
                tokio::spawn(send_notification(alert));
            }
            store.update_battery(state).await;
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
}

async fn display_device_path(connection: &zbus::Connection) -> Result<OwnedObjectPath> {
    let proxy = zbus::Proxy::new(connection, BUS, ROOT_PATH, ROOT_INTERFACE).await?;
    proxy
        .get_property("DisplayDevice")
        .await
        .context("read UPower display device")
}

async fn read_state(connection: &zbus::Connection, path: &OwnedObjectPath) -> Result<BatteryState> {
    let root = zbus::Proxy::new(connection, BUS, ROOT_PATH, ROOT_INTERFACE).await?;
    let device = zbus::Proxy::new(connection, BUS, path.as_str(), DEVICE_INTERFACE).await?;
    let present: bool = device.get_property("IsPresent").await.unwrap_or(false);
    let device_type: u32 = device.get_property("Type").await.unwrap_or(0);
    let native_path: String = device.get_property("NativePath").await.unwrap_or_default();
    let percentage = device
        .get_property::<f64>("Percentage")
        .await
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 100.0) as u8;
    let state_code: u32 = device.get_property("State").await.unwrap_or(0);
    let on_battery: bool = root.get_property("OnBattery").await.unwrap_or(false);
    let health = device
        .get_property::<f64>("Capacity")
        .await
        .ok()
        .map(|value| value.round().clamp(0.0, 100.0) as u8);
    Ok(BatteryState {
        available: present && device_type == 2,
        native_path: native_path.clone(),
        percentage,
        state: state_name(state_code).into(),
        charging: matches!(state_code, 1 | 5),
        plugged: !on_battery,
        power_watts: device
            .get_property::<f64>("EnergyRate")
            .await
            .unwrap_or(0.0),
        time_to_empty_seconds: device.get_property("TimeToEmpty").await.unwrap_or(0),
        time_to_full_seconds: device.get_property("TimeToFull").await.unwrap_or(0),
        health_percent: health,
        cycles: read_cycle_count(&native_path),
        warning: on_battery && percentage <= WARNING_PERCENT,
        critical: on_battery && percentage <= CRITICAL_PERCENT,
        error: None,
    })
}

fn state_name(state: u32) -> &'static str {
    match state {
        1 => "charging",
        2 => "discharging",
        3 => "empty",
        4 => "fully-charged",
        5 => "pending-charge",
        6 => "pending-discharge",
        _ => "unknown",
    }
}

fn read_cycle_count(native_path: &str) -> Option<u32> {
    if native_path.is_empty() {
        return None;
    }
    let path = Path::new("/sys/class/power_supply")
        .join(native_path)
        .join("cycle_count");
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BatteryAlert {
    Warning,
    Critical,
    Full,
}

#[derive(Default)]
struct AlertTracker {
    initialized: bool,
    warning_sent: bool,
    critical_sent: bool,
    full_sent: bool,
}

impl AlertTracker {
    fn observe(&mut self, state: &BatteryState) -> Option<BatteryAlert> {
        if !state.available {
            self.initialized = true;
            return None;
        }
        if !self.initialized {
            self.initialized = true;
            self.warning_sent = state.warning;
            self.critical_sent = state.critical;
            self.full_sent = state.plugged && state.percentage >= 100;
            return None;
        }
        if state.plugged {
            self.warning_sent = false;
            self.critical_sent = false;
            if state.percentage < 100 {
                self.full_sent = false;
            }
            if state.percentage >= 100 && !self.full_sent {
                self.full_sent = true;
                return Some(BatteryAlert::Full);
            }
            return None;
        }
        self.full_sent = false;
        if state.critical && !self.critical_sent {
            self.critical_sent = true;
            self.warning_sent = true;
            return Some(BatteryAlert::Critical);
        }
        if state.warning && !self.warning_sent {
            self.warning_sent = true;
            return Some(BatteryAlert::Warning);
        }
        None
    }
}

async fn send_notification(alert: BatteryAlert) {
    let result = async {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await?;
        let (icon, summary, body, urgency) = match alert {
            BatteryAlert::Warning => ("battery-caution", "Battery low", "Plug in soon.", 1u8),
            BatteryAlert::Critical => ("battery-empty", "Battery critical", "Plug in now.", 2u8),
            BatteryAlert::Full => (
                "battery-full-charged",
                "Battery full",
                "Unplug to reduce wear.",
                0u8,
            ),
        };
        let mut hints = HashMap::<String, OwnedValue>::new();
        hints.insert("urgency".into(), OwnedValue::from(urgency));
        let _: u32 = proxy
            .call(
                "Notify",
                &(
                    "bar-daemon",
                    0u32,
                    icon,
                    summary,
                    body,
                    Vec::<String>::new(),
                    hints,
                    -1i32,
                ),
            )
            .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "battery notification failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{AlertTracker, BatteryAlert, state_name};
    use crate::model::BatteryState;

    fn state(percent: u8, plugged: bool) -> BatteryState {
        BatteryState {
            available: true,
            percentage: percent,
            plugged,
            warning: !plugged && percent <= 25,
            critical: !plugged && percent <= 12,
            ..BatteryState::default()
        }
    }

    #[test]
    fn alerts_once_per_discharge_cycle_without_startup_noise() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(50, false)), None);
        assert_eq!(
            tracker.observe(&state(25, false)),
            Some(BatteryAlert::Warning)
        );
        assert_eq!(tracker.observe(&state(20, false)), None);
        assert_eq!(
            tracker.observe(&state(12, false)),
            Some(BatteryAlert::Critical)
        );
        assert_eq!(tracker.observe(&state(10, false)), None);
        assert_eq!(tracker.observe(&state(50, true)), None);
        assert_eq!(
            tracker.observe(&state(25, false)),
            Some(BatteryAlert::Warning)
        );
    }

    #[test]
    fn reports_full_only_after_initial_state() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(100, true)), None);
        assert_eq!(tracker.observe(&state(99, true)), None);
        assert_eq!(tracker.observe(&state(100, true)), Some(BatteryAlert::Full));
    }

    #[test]
    fn maps_upower_states() {
        assert_eq!(state_name(1), "charging");
        assert_eq!(state_name(4), "fully-charged");
        assert_eq!(state_name(99), "unknown");
    }
}
