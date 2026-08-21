use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::time::{interval, sleep};
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
const HOLD_REASON: &str = "Battery level is low";

pub async fn monitor(store: StateStore) {
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
    let properties = zbus::Proxy::new(connection, BUS, PATH, PROPERTIES_INTERFACE).await?;
    let mut changes = properties.receive_signal("PropertiesChanged").await?;
    let mut events = store.subscribe();
    let mut fallback = interval(Duration::from_secs(60));
    let mut low_battery_hold = LowBatteryHold::default();
    fallback.tick().await;
    refresh(connection, store).await;
    low_battery_hold
        .reconcile(connection, &store.snapshot().await.battery)
        .await?;
    loop {
        tokio::select! {
            signal = changes.next() => {
                if signal.is_none() { bail!("power-profiles-daemon property stream ended"); }
                refresh(connection, store).await;
            }
            event = events.recv() => {
                match event {
                    Ok(event) if event.stream == protocol::stream::BATTERY => {
                        low_battery_hold
                            .reconcile(connection, &store.snapshot().await.battery)
                            .await?;
                        refresh(connection, store).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        low_battery_hold
                            .reconcile(connection, &store.snapshot().await.battery)
                            .await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        bail!("state event stream ended");
                    }
                }
            }
            _ = fallback.tick() => {
                low_battery_hold
                    .reconcile(connection, &store.snapshot().await.battery)
                    .await?;
                refresh(connection, store).await;
            },
        }
    }
}

#[derive(Default)]
struct LowBatteryHold {
    cookie: Option<u32>,
}

impl LowBatteryHold {
    async fn reconcile(
        &mut self,
        connection: &zbus::Connection,
        battery: &BatteryState,
    ) -> Result<()> {
        let desired = should_hold_power_saver(battery);
        if !desired {
            return self.release(connection).await;
        }
        if self.cookie.is_some() {
            return Ok(());
        }
        let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE)
            .await
            .context("connect to power-profiles-daemon for low-battery hold")?;
        let cookie: u32 = proxy
            .call(
                "HoldProfile",
                &("power-saver", HOLD_REASON, HOLD_APPLICATION_ID),
            )
            .await
            .context("hold power-saver for low battery")?;
        self.cookie = Some(cookie);
        Ok(())
    }

    async fn release(&mut self, connection: &zbus::Connection) -> Result<()> {
        let Some(cookie) = self.cookie.take() else {
            return Ok(());
        };
        let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE)
            .await
            .context("connect to power-profiles-daemon to release low-battery hold")?;
        let _: () = proxy
            .call("ReleaseProfile", &(cookie,))
            .await
            .context("release low-battery power-saver hold")?;
        Ok(())
    }
}

fn should_hold_power_saver(battery: &BatteryState) -> bool {
    battery.available && battery.policy.auto_power_saver && battery.warning && !battery.plugged
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
        name: property_string(values, "Action")?,
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

pub async fn set_profile(profile: &str) -> Result<PowerProfileState> {
    if !matches!(profile, "power-saver" | "balanced" | "performance") {
        bail!("unsupported power profile: {profile}");
    }
    let connection = zbus::Connection::system()
        .await
        .context("connect to system D-Bus")?;
    let proxy = zbus::Proxy::new(&connection, BUS, PATH, INTERFACE)
        .await
        .context("connect to power-profiles-daemon")?;
    let current = read_state(&connection).await?;
    if !current.profiles.iter().any(|item| item.name == profile) {
        bail!("power profile is unavailable: {profile}");
    }
    proxy
        .set_property("ActiveProfile", &profile)
        .await
        .context("set active power profile")?;
    read_state(&connection).await
}

pub async fn set_battery_aware(enabled: bool) -> Result<PowerProfileState> {
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

pub async fn set_action_enabled(action: &str, enabled: bool) -> Result<PowerProfileState> {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use zvariant::{OwnedValue, Value};

    use crate::model::{BatteryPolicyState, BatteryState};

    use super::{parse_action, parse_hold, parse_profile, should_hold_power_saver};

    fn owned(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::new(value)).unwrap()
    }

    #[test]
    fn parses_profile_dictionary() {
        let values = HashMap::from([
            ("Profile".into(), owned("balanced")),
            ("Driver".into(), owned("amd_pstate")),
            ("PlatformDriver".into(), owned("platform_profile")),
        ]);
        let profile = parse_profile(&values).unwrap();
        assert_eq!(profile.name, "balanced");
        assert_eq!(profile.driver, "amd_pstate");
    }

    #[test]
    fn parses_action_and_hold_dictionaries() {
        let action = HashMap::from([
            ("Action".into(), owned("amdgpu_panel_power")),
            ("Description".into(), owned("Panel power savings")),
            (
                "Enabled".into(),
                OwnedValue::try_from(Value::new(true)).unwrap(),
            ),
        ]);
        let action = parse_action(&action).unwrap();
        assert_eq!(action.name, "amdgpu_panel_power");
        assert!(action.enabled);

        let hold = HashMap::from([
            ("ApplicationId".into(), owned("org.example.Compiler")),
            ("Profile".into(), owned("performance")),
            ("Reason".into(), owned("Building")),
        ]);
        let hold = parse_hold(&hold).unwrap();
        assert_eq!(hold.profile, "performance");
        assert_eq!(hold.reason, "Building");
    }

    #[test]
    fn low_battery_hold_requires_enabled_discharging_warning() {
        let state = BatteryState {
            available: true,
            warning: true,
            policy: BatteryPolicyState {
                auto_power_saver: true,
                ..BatteryPolicyState::default()
            },
            ..BatteryState::default()
        };
        assert!(should_hold_power_saver(&state));
        assert!(!should_hold_power_saver(&BatteryState {
            plugged: true,
            ..state.clone()
        }));
        assert!(!should_hold_power_saver(&BatteryState {
            warning: false,
            ..state
        }));
    }
}
