use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;
use futures::{StreamExt, TryFutureExt};
use tokio::{
    io::AsyncReadExt,
    time::{Instant, MissedTickBehavior, interval_at, sleep},
};

use crate::{
    model::{AcceptedLocation, LocationControl, TimezoneState},
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

// Both readers retain verified values on failure and only retry when invalidated or due.
struct Cached<T> {
    value: T,
    error: Option<String>,
    retry: Retry,
    pending: bool,
}
impl<T> Cached<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            error: None,
            retry: Retry::default(),
            pending: true,
        }
    }

    async fn refresh(
        &mut self,
        read: impl std::future::Future<Output = Result<T>>,
        base: Duration,
    ) -> bool {
        if !self.pending {
            return false;
        }
        let result = read.await;
        self.pending = false;
        self.retry.completed(result.is_ok(), base);
        match result {
            Ok(value) => {
                self.value = value;
                self.error = None;
            }
            Err(error) => {
                tracing::debug!(%error, "timezone observation unavailable; retaining verified data");
                self.error = Some(error.to_string());
            }
        }
        true
    }
}

type LocationSnapshot = (Option<AcceptedLocation>, Option<LocationControl>);
struct Observations {
    zone: Cached<Option<String>>,
    location: Cached<LocationSnapshot>,
    status: PublicStatus,
}
impl Observations {
    fn project(&self, now: DateTime<Utc>) -> TimezoneState {
        let mut state = self
            .zone
            .value
            .as_deref()
            .and_then(|name| state_for_timezone(name, now).ok())
            .unwrap_or_default();
        // Cached timezone data is useful for recovery, but a failed read is not verified.
        state.available &= self.zone.error.is_none();
        state.error = self
            .zone
            .error
            .clone()
            .or_else(|| self.status.message.clone());
        state.accepted_location = self.location.value.0.clone();
        state.location_control = self.location.value.1.clone();
        state
    }

    fn update_status(&mut self, next: PublicStatus) -> bool {
        if next == self.status {
            return false;
        }
        // Revisions catch accepted updates even if checking -> empty happened between polls.
        // A same-revision message change is presentation only; legacy messages still work.
        self.location.pending |= next.revision != self.status.revision || next.revision.is_none();
        self.status = next;
        true
    }
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
    let mut observed = Observations {
        status: location_status(status_path).await,
        zone: Cached::new(None),
        location: Cached::new((
            store.read(|s| s.timezone.accepted_location.clone()).await,
            None,
        )),
    };
    let mut dirty = true;
    loop {
        dirty |= observed
            .zone
            .refresh(read_timezone(connection).map_ok(Some), timing.retry)
            .await;
        dirty |= observed
            .location
            .refresh(read_location(connection), timing.retry)
            .await;
        if dirty {
            store.update_timezone(observed.project(Utc::now())).await;
            dirty = false;
        }
        tokio::select! {
            signal = changes.next() => {
                let signal = signal.context("timedate1 property stream ended")?;
                observed.zone.pending |= timezone_changed(&signal)?;
            }
            signal = owners.next() => {
                let signal = signal.context("D-Bus owner stream ended")?;
                let args = signal.args()?;
                // Idle timedated disappearance is normal, not an error or reason
                // to reactivate it. Re-read when an external caller starts it.
                if args.new_owner().as_ref().is_some() {
                    observed.zone.pending |= args.name().as_str() == BUS;
                    observed.location.pending |= args.name().as_str() == LOCATION;
                }
            }
            _ = local_tick.tick() => dirty = true,
            _ = status_poll.tick() => {
                dirty |= observed.update_status(location_status(status_path).await);
                observed.zone.pending |= observed.zone.retry.ready();
                observed.location.pending |= observed.location.retry.ready();
            }
        }
    }
}

fn timezone_changed(signal: &zbus::Message) -> Result<bool> {
    let (interface, changed, invalidated): (
        String,
        std::collections::HashMap<String, zvariant::OwnedValue>,
        Vec<String>,
    ) = signal.body().deserialize()?;
    Ok(interface == BUS
        && (changed.contains_key("Timezone") || invalidated.iter().any(|s| s == "Timezone")))
}

// Read the control revision on both sides of the accepted fix. An older service
// without GetControl remains supported; a torn modern snapshot is retried.
async fn read_location(connection: &zbus::Connection) -> Result<LocationSnapshot> {
    let control = read_control(connection).await.ok();
    let location = accepted_location(connection).await;
    if let Some(before) = &control {
        anyhow::ensure!(
            read_control(connection)
                .await
                .is_ok_and(|after| before.revision == after.revision),
            "Location changed during read"
        );
    }
    Ok((location?, control))
}

pub(crate) async fn read_control(
    connection: &zbus::Connection,
) -> Result<crate::model::LocationControl> {
    let json = location_call(connection, "GetControl", None).await?;
    parse_control(&json)
}
fn parse_control(json: &str) -> Result<crate::model::LocationControl> {
    anyhow::ensure!(json.len() <= 8192, "Location control response too large");
    let result: crate::model::LocationControl = serde_json::from_str(json)?;
    if let Some(place) = &result.effective_place {
        place.validate()?;
    }
    Ok(result)
}
async fn location_call(
    connection: &zbus::Connection,
    method: &str,
    params: Option<String>,
) -> Result<String> {
    tokio::time::timeout(
        Duration::from_secs(if params.is_some() { 30 } else { 5 }),
        async {
            let proxy = zbus::Proxy::new(
                connection,
                LOCATION,
                "/org/laufan/LocationTimezone",
                LOCATION,
            )
            .await?;
            Ok::<_, anyhow::Error>(if let Some(params) = params {
                proxy.call(method, &(params,)).await?
            } else {
                proxy.call(method, &()).await?
            })
        },
    )
    .await
    .context("Location request timed out; read status before any retry")?
}
pub(crate) async fn location_control(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let connection = zbus::Connection::system().await?;
    let value = if method == "location.status" {
        read_control(&connection).await?
    } else {
        #[derive(serde::Deserialize, serde::Serialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            request_id: String,
            revision: u64,
            place: Option<crate::location_place::Place>,
        }
        let mut request: Request = serde_json::from_value(params)?;
        if method == "location.returnToAutomatic" {
            anyhow::ensure!(request.place.is_none(), "Automatic does not accept a place");
        } else {
            request
                .place
                .as_mut()
                .context("Manual location requires a place")?
                .validate()?;
        }
        let json = location_call(
            &connection,
            "SetLocationPolicy",
            Some(serde_json::to_string(&request)?),
        )
        .await?;
        parse_control(&json)?
    };
    Ok(serde_json::json!({"location_control":value}))
}

async fn accepted_location(connection: &zbus::Connection) -> Result<Option<AcceptedLocation>> {
    parse_accepted_location(&location_call(connection, "GetAccepted", None).await?)
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
        location_control: None,
        error: None,
    })
}

#[cfg(test)]
mod tests;
