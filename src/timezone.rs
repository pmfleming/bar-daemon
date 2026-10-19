use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;
use futures::StreamExt;
use tokio::{
    io::AsyncReadExt,
    time::{Instant, MissedTickBehavior, interval_at, sleep},
};

use crate::{
    model::{AcceptedLocation, TimezoneState},
    state::StateStore,
};

const BUS: &str = "org.freedesktop.timedate1";
const PATH: &str = "/org/freedesktop/timedate1";
const LOCATION: &str = "org.laufan.LocationTimezone";
const STATUS: &str = "/run/location-timezone/status.json";

pub(crate) async fn monitor(store: StateStore) {
    loop {
        let result = match zbus::Connection::system().await {
            Ok(connection) => {
                monitor_connection(&connection, &store, Path::new(STATUS), Timing::default()).await
            }
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

#[derive(Clone, Copy)]
struct Timing {
    status: Duration,
    local: Duration,
    retry: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            status: Duration::from_secs(2),
            local: Duration::from_secs(60),
            retry: Duration::from_secs(5),
        }
    }
}

#[derive(Default)]
struct Retry {
    due: Option<Instant>,
    failures: u32,
}
impl Retry {
    fn ready(&self) -> bool {
        self.due.is_some_and(|due| due <= Instant::now())
    }
    fn completed(&mut self, success: bool, base: Duration) {
        if success {
            *self = Self::default();
        } else {
            let delay = base
                .saturating_mul(2_u32.pow(self.failures))
                .min(Duration::from_secs(300));
            self.failures = (self.failures + 1).min(16);
            self.due = Some(Instant::now() + delay);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PublicStatus {
    message: Option<String>,
    revision: Option<u64>,
}

async fn monitor_connection(
    connection: &zbus::Connection,
    store: &StateStore,
    status_path: &Path,
    timing: Timing,
) -> Result<()> {
    // Subscribe before reading. Neither listener needs periodic service activation.
    let properties: zbus::Proxy<'_> = zbus::proxy::Builder::new(connection)
        .destination(BUS)?
        .path(PATH)?
        .interface("org.freedesktop.DBus.Properties")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let mut changes = properties.receive_signal("PropertiesChanged").await?;
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let mut owners = dbus.receive_name_owner_changed().await?;
    let mut status_poll = interval_at(Instant::now() + timing.status, timing.status);
    let mut local_tick = interval_at(Instant::now() + timing.local, timing.local);
    status_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    local_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut status = location_status(status_path).await;
    let mut zone: Option<String> = None;
    let mut zone_error = None;
    let mut accepted = store.read(|s| s.timezone.accepted_location.clone()).await;
    let mut zone_retry = Retry::default();
    let mut accepted_retry = Retry::default();
    let (mut read_zone, mut read_accepted, mut dirty) = (true, true, true);
    loop {
        if read_zone {
            let result = read_timezone(connection).await;
            zone_retry.completed(result.is_ok(), timing.retry);
            match result {
                Ok(name) => {
                    zone = Some(name);
                    zone_error = None;
                }
                Err(error) => {
                    zone_error = Some(error.to_string());
                }
            }
            dirty = true;
            read_zone = false;
        }
        if read_accepted {
            let result = accepted_location(connection).await;
            accepted_retry.completed(result.is_ok(), timing.retry);
            match result {
                Ok(location) => accepted = location,
                Err(error) => {
                    tracing::debug!(%error, "accepted location reader unavailable; retaining last verified fix")
                }
            }
            dirty = true;
            read_accepted = false;
        }
        if dirty {
            let mut state = zone
                .as_deref()
                .and_then(|name| state_for_timezone(name, Utc::now()).ok())
                .unwrap_or_default();
            // Retain cached data for recovery, but don't claim a failed read is verified.
            if zone_error.is_some() {
                state.available = false;
            }
            state.error = zone_error.clone().or_else(|| status.message.clone());
            state.accepted_location = accepted.clone();
            store.update_timezone(state).await;
            dirty = false;
        }
        tokio::select! {
            signal = changes.next() => {
                let signal = signal.context("timedate1 property stream ended")?;
                let (interface, changed, invalidated): (String, std::collections::HashMap<String, zvariant::OwnedValue>, Vec<String>) = signal.body().deserialize()?;
                if interface == BUS && (changed.contains_key("Timezone") || invalidated.iter().any(|s| s == "Timezone")) {
                    read_zone = true;
                }
            }
            signal = owners.next() => {
                let signal = signal.context("D-Bus owner stream ended")?;
                let args = signal.args()?;
                // Idle timedated disappearance is normal, not an error or reason
                // to reactivate it. Re-read when an external caller starts it.
                if args.new_owner().as_ref().is_some() {
                    match args.name().as_str() {
                        BUS => read_zone = true,
                        LOCATION => read_accepted = true,
                        _ => {},
                    }
                }
            }
            _ = local_tick.tick() => dirty = true,
            _ = status_poll.tick() => {
                let next = location_status(status_path).await;
                if next != status {
                    // Revisions catch accepted updates even if checking -> empty
                    // occurred entirely between polls. Legacy messages still work.
                    if next.revision != status.revision || next.revision.is_none() {
                        read_accepted = true;
                    }
                    status = next;
                    dirty = true;
                }
                read_zone |= zone_retry.ready();
                read_accepted |= accepted_retry.ready();
            }
        }
    }
}

async fn accepted_location(connection: &zbus::Connection) -> Result<Option<AcceptedLocation>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(connection)
            .destination(LOCATION)?
            .path("/org/laufan/LocationTimezone")?
            .interface(LOCATION)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let json: String = proxy.call("GetAccepted", &()).await?;
        parse_accepted_location(&json)
    })
    .await
    .context("accepted location read timed out")?
}

fn parse_accepted_location(json: &str) -> Result<Option<AcceptedLocation>> {
    anyhow::ensure!(json.len() <= 4096, "accepted location response too large");
    let location: Option<AcceptedLocation> = serde_json::from_str(json)?;
    if let Some(fix) = &location {
        anyhow::ensure!(
            (-90.0..=90.0).contains(&fix.latitude)
                && (-180.0..=180.0).contains(&fix.longitude)
                && fix.verified_at.is_finite()
                && fix.verified_at > 0.0
                && fix.timezone.parse::<Tz>().is_ok()
                && fix
                    .accuracy_meters
                    .is_none_or(|a| a.is_finite() && a >= 0.0),
            "invalid accepted location"
        );
    }
    Ok(location)
}

async fn read_timezone(connection: &zbus::Connection) -> Result<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(connection)
            .destination(BUS)?
            .path(PATH)?
            .interface(BUS)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let name: String = proxy
            .get_property("Timezone")
            .await
            .context("read system timezone")?;
        name.parse::<Tz>().context("parse system timezone")?;
        Ok(name)
    })
    .await
    .context("system timezone read timed out")?
}

async fn location_status(path: &Path) -> PublicStatus {
    let Ok(file) = tokio::fs::File::open(path).await else {
        return PublicStatus::default();
    };
    let mut bytes = Vec::new();
    if file.take(4097).read_to_end(&mut bytes).await.is_err() {
        return PublicStatus::default();
    }
    parse_location_status(&bytes)
}

fn parse_location_status(bytes: &[u8]) -> PublicStatus {
    #[derive(serde::Deserialize)]
    struct Status {
        message: String,
        #[serde(default)]
        accepted_revision: Option<u64>,
    }
    if bytes.len() <= 4096
        && let Ok(status) = serde_json::from_slice::<Status>(bytes)
    {
        PublicStatus {
            message: (!status.message.trim().is_empty())
                .then(|| status.message.chars().take(300).collect()),
            revision: status.accepted_revision,
        }
    } else {
        PublicStatus {
            message: Some("Location status unavailable".into()),
            revision: None,
        }
    }
}

fn state_for_timezone(timezone: &str, instant: DateTime<Utc>) -> Result<TimezoneState> {
    let zone: Tz = timezone
        .parse()
        .with_context(|| format!("parse timezone {timezone}"))?;
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
mod tests;
