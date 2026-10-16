use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Offset, Utc};
use chrono_tz::Tz;
use futures::StreamExt;
use tokio::{
    io::AsyncReadExt,
    time::{interval, sleep},
};

use crate::{
    model::{AcceptedLocation, TimezoneState},
    state::StateStore,
};

const BUS: &str = "org.freedesktop.timedate1";
const PATH: &str = "/org/freedesktop/timedate1";
const INTERFACE: &str = "org.freedesktop.timedate1";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

pub(crate) async fn monitor(store: StateStore) {
    loop {
        let result = match zbus::Connection::system().await {
            Ok(connection) => monitor_connection(&connection, &store).await,
            Err(error) => Err(error.into()),
        };
        if let Err(error) = result {
            store
                .update_timezone(TimezoneState {
                    error: Some(error.to_string()),
                    accepted_location: store.read(|s| s.timezone.accepted_location.clone()).await,
                    ..TimezoneState::default()
                })
                .await;
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn monitor_connection(connection: &zbus::Connection, store: &StateStore) -> Result<()> {
    let properties = zbus::Proxy::new(connection, BUS, PATH, PROPERTIES_INTERFACE).await?;
    let mut changes = properties.receive_signal("PropertiesChanged").await?;
    // Also discover accepted fixes when the timezone itself has not changed.
    let mut fallback = interval(Duration::from_secs(10));
    fallback.tick().await;
    refresh(connection, store).await;
    // The system location authority publishes only a concise public status;
    // coordinates and quota state remain in its private StateDirectory.
    let mut status_poll = interval(Duration::from_secs(2));
    let mut previous_status = location_warning().await;
    loop {
        tokio::select! {
            signal = changes.next() => {
                if signal.is_none() { bail!("timedate1 property stream ended"); }
                refresh(connection, store).await;
            }
            _ = fallback.tick() => refresh(connection, store).await,
            _ = status_poll.tick() => {
                let status = location_warning().await;
                if status != previous_status {
                    previous_status = status;
                    refresh(connection, store).await;
                }
            }
        }
    }
}

async fn refresh(connection: &zbus::Connection, store: &StateStore) {
    let mut state = read_state(connection)
        .await
        .unwrap_or_else(|error| TimezoneState {
            error: Some(error.to_string()),
            ..TimezoneState::default()
        });
    state.accepted_location = match accepted_location(connection).await {
        Ok(location) => location,
        // An optional service outage must not discard the last verified fix.
        Err(error) => {
            tracing::debug!(error = %error, "accepted location reader unavailable; retaining last verified fix");
            store.read(|s| s.timezone.accepted_location.clone()).await
        }
    };
    store.update_timezone(state).await;
}

async fn accepted_location(connection: &zbus::Connection) -> Result<Option<AcceptedLocation>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let proxy = zbus::Proxy::new(
            connection,
            "org.laufan.LocationTimezone",
            "/org/laufan/LocationTimezone",
            "org.laufan.LocationTimezone",
        )
        .await?;
        let json: String = proxy.call("GetAccepted", &()).await?;
        anyhow::ensure!(json.len() <= 4096, "accepted location response too large");
        let location: Option<AcceptedLocation> = serde_json::from_str(&json)?;
        if let Some(fix) = &location {
            anyhow::ensure!(
                (-90.0..=90.0).contains(&fix.latitude)
                    && (-180.0..=180.0).contains(&fix.longitude)
                    && fix.verified_at.is_finite()
                    && fix.verified_at > 0.0
                    && fix.timezone.parse::<Tz>().is_ok(),
                "invalid accepted location"
            );
        }
        Ok(location)
    })
    .await
    .context("accepted location read timed out")?
}

async fn read_state(connection: &zbus::Connection) -> Result<TimezoneState> {
    let proxy = zbus::Proxy::new(connection, BUS, PATH, INTERFACE)
        .await
        .context("connect to systemd-timedated")?;
    let timezone: String = proxy
        .get_property("Timezone")
        .await
        .context("read system timezone")?;
    let mut state = state_for_timezone(&timezone)?;
    state.error = location_warning().await;
    Ok(state)
}

async fn location_warning() -> Option<String> {
    let file = tokio::fs::File::open("/run/location-timezone/status.json")
        .await
        .ok()?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).await.ok()?;
    parse_location_warning(&bytes)
}

fn parse_location_warning(bytes: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Status {
        message: String,
    }
    if bytes.len() > 4096 {
        return Some("Location status unavailable".into());
    }
    match serde_json::from_slice::<Status>(bytes) {
        Ok(status) if status.message.trim().is_empty() => None,
        Ok(status) => Some(status.message.chars().take(300).collect()),
        Err(_) => Some("Location status unavailable".into()),
    }
}

fn state_for_timezone(timezone: &str) -> Result<TimezoneState> {
    let zone: Tz = timezone
        .parse()
        .with_context(|| format!("parse timezone {timezone}"))?;
    let instant = Utc::now();
    let now = instant.with_timezone(&zone);
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
        timezone_region_ids: crate::timezone_regions::ids_for_offset(
            now.offset().fix().local_minus_utc(),
            instant,
        ),
        accepted_location: None,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_location_warning;

    #[test]
    fn location_status_is_optional_bounded_and_can_clear() {
        assert_eq!(
            parse_location_warning(br#"{"message":"Sources disagree"}"#),
            Some("Sources disagree".into())
        );
        assert_eq!(parse_location_warning(br#"{"message":""}"#), None);
        assert_eq!(
            parse_location_warning(b"broken"),
            Some("Location status unavailable".into())
        );
        assert_eq!(
            parse_location_warning(&vec![b' '; 4097]),
            Some("Location status unavailable".into())
        );
    }
}
