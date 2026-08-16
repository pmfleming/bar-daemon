use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Offset, Utc};
use chrono_tz::Tz;
use futures::StreamExt;
use tokio::time::{interval, sleep};

use crate::{model::TimezoneState, state::StateStore};

const BUS: &str = "org.freedesktop.timedate1";
const PATH: &str = "/org/freedesktop/timedate1";
const INTERFACE: &str = "org.freedesktop.timedate1";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

pub async fn monitor(store: StateStore) {
    loop {
        match zbus::Connection::system().await {
            Ok(connection) => {
                if let Err(error) = monitor_connection(&connection, &store).await {
                    store
                        .update_timezone(TimezoneState {
                            error: Some(error.to_string()),
                            ..TimezoneState::default()
                        })
                        .await;
                }
            }
            Err(error) => {
                store
                    .update_timezone(TimezoneState {
                        error: Some(error.to_string()),
                        ..TimezoneState::default()
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
    let mut fallback = interval(Duration::from_secs(300));
    fallback.tick().await;
    refresh(connection, store).await;
    loop {
        tokio::select! {
            signal = changes.next() => {
                if signal.is_none() { bail!("timedate1 property stream ended"); }
                refresh(connection, store).await;
            }
            _ = fallback.tick() => refresh(connection, store).await,
        }
    }
}

async fn refresh(connection: &zbus::Connection, store: &StateStore) {
    match read_state(connection).await {
        Ok(state) => store.update_timezone(state).await,
        Err(error) => {
            store
                .update_timezone(TimezoneState {
                    error: Some(error.to_string()),
                    ..TimezoneState::default()
                })
                .await
        }
    }
}

async fn read_state(connection: &zbus::Connection) -> Result<TimezoneState> {
    let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE)
        .await
        .context("connect to systemd-timedated")?;
    let timezone: String = proxy
        .get_property("Timezone")
        .await
        .context("read system timezone")?;
    state_for_timezone(&timezone)
}

fn state_for_timezone(timezone: &str) -> Result<TimezoneState> {
    let zone: Tz = timezone
        .parse()
        .with_context(|| format!("parse timezone {timezone}"))?;
    let now = Utc::now().with_timezone(&zone);
    Ok(TimezoneState {
        available: true,
        timezone: timezone.into(),
        city: timezone
            .rsplit('/')
            .next()
            .unwrap_or(timezone)
            .replace('_', " "),
        abbreviation: now.format("%Z").to_string(),
        utc_offset_seconds: now.offset().fix().local_minus_utc(),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::state_for_timezone;

    #[test]
    fn derives_human_city_and_offset() {
        let state = state_for_timezone("Europe/Amsterdam").unwrap();
        assert_eq!(state.city, "Amsterdam");
        assert!([3600, 7200].contains(&state.utc_offset_seconds));
        assert!(!state.abbreviation.is_empty());
    }

    #[test]
    fn replaces_underscores_in_city() {
        assert_eq!(
            state_for_timezone("America/New_York").unwrap().city,
            "New York"
        );
    }
}
