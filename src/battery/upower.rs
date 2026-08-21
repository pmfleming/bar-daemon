use anyhow::{Context, Result};
use zvariant::OwnedObjectPath;

use crate::model::BatteryState;

use super::{CRITICAL_PERCENT, WARNING_PERCENT, sysfs};

pub(super) const BUS: &str = "org.freedesktop.UPower";
pub(super) const ROOT_PATH: &str = "/org/freedesktop/UPower";
pub(super) const ROOT_INTERFACE: &str = "org.freedesktop.UPower";
pub(super) const DEVICE_INTERFACE: &str = "org.freedesktop.UPower.Device";
pub(super) const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

pub(super) async fn display_device_path(connection: &zbus::Connection) -> Result<OwnedObjectPath> {
    let proxy = zbus::Proxy::new(connection, BUS, ROOT_PATH, ROOT_INTERFACE).await?;
    proxy
        .get_property("DisplayDevice")
        .await
        .context("read UPower display device")
}

pub(super) async fn read_state(
    connection: &zbus::Connection,
    path: &OwnedObjectPath,
) -> Result<BatteryState> {
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
        cycles: sysfs::read_cycle_count(&native_path),
        warning: on_battery && percentage <= WARNING_PERCENT,
        critical: on_battery && percentage <= CRITICAL_PERCENT,
        protection: Default::default(),
        devices: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::state_name;

    #[test]
    fn maps_upower_states() {
        assert_eq!(state_name(1), "charging");
        assert_eq!(state_name(4), "fully-charged");
        assert_eq!(state_name(99), "unknown");
    }
}
