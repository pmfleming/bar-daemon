use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::time::{interval, sleep};
use zvariant::OwnedValue;

use crate::{
    model::{PowerProfile, PowerProfileState},
    state::StateStore,
};

const BUS: &str = "org.freedesktop.UPower.PowerProfiles";
const PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
const INTERFACE: &str = "org.freedesktop.UPower.PowerProfiles";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

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
    let mut fallback = interval(Duration::from_secs(60));
    fallback.tick().await;
    refresh(connection, store).await;
    loop {
        tokio::select! {
            signal = changes.next() => {
                if signal.is_none() { bail!("power-profiles-daemon property stream ended"); }
                refresh(connection, store).await;
            }
            _ = fallback.tick() => refresh(connection, store).await,
        }
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
    Ok(PowerProfileState {
        available: true,
        profile,
        driver,
        profiles,
        performance_degraded: proxy
            .get_property("PerformanceDegraded")
            .await
            .unwrap_or_default(),
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

fn property_string(values: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    values
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .map(str::to_string)
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use zvariant::{OwnedValue, Value};

    use super::parse_profile;

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
}
