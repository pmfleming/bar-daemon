use super::{
    Backend, Control, Observation, http,
    policy::{Fix, Network},
    wall_time,
};
use anyhow::{Context, Result, bail, ensure};
use futures::StreamExt;
use std::{collections::HashMap, time::Duration};
use zbus::{Connection, Proxy};
use zvariant::{OwnedObjectPath, OwnedValue};

const NM: &str = "org.freedesktop.NetworkManager";
const GEO: &str = "org.freedesktop.GeoClue2";
const GEO_MANAGER: &str = "/org/freedesktop/GeoClue2/Manager";
const TIME: &str = "org.freedesktop.timedate1";
const LOGIN: &str = "org.freedesktop.login1";
const IO: Duration = Duration::from_secs(5);

pub(super) struct Bus {
    pub connection: Connection,
    pub http: reqwest::Client,
}
async fn proxy<'a>(
    connection: &'a Connection,
    name: &'a str,
    path: &'a str,
    interface: &'a str,
) -> Result<Proxy<'a>> {
    Ok(zbus::proxy::Builder::new(connection)
        .destination(name)?
        .path(path)?
        .interface(interface)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?)
}
async fn properties(
    connection: &Connection,
    name: &str,
    path: &str,
    interface: &str,
) -> Result<HashMap<String, OwnedValue>> {
    Ok(
        proxy(connection, name, path, "org.freedesktop.DBus.Properties")
            .await?
            .call("GetAll", &(interface,))
            .await?,
    )
}
fn field<'a>(values: &'a HashMap<String, OwnedValue>, name: &str) -> Result<&'a OwnedValue> {
    values
        .get(name)
        .with_context(|| format!("missing D-Bus property {name}"))
}
impl Backend for Bus {
    async fn observe(&self) -> Result<Observation> {
        tokio::time::timeout(IO, async {
            let login = proxy(
                &self.connection,
                LOGIN,
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            )
            .await?;
            let sleeping: bool = login.get_property("PreparingForSleep").await?;
            let dbus = zbus::fdo::DBusProxy::new(&self.connection).await?;
            let owner = dbus.get_name_owner(NM.try_into()?).await?.to_string();
            let values =
                properties(&self.connection, NM, "/org/freedesktop/NetworkManager", NM).await?;
            if u32::try_from(field(&values, "Connectivity")?)? != 4
                || u32::try_from(field(&values, "State")?)? != 70
            {
                return Ok(Observation {
                    network: None,
                    sleeping,
                });
            }
            let mut active = Vec::<OwnedObjectPath>::try_from(
                field(&values, "ActiveConnections")?.try_clone()?,
            )?
            .into_iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>();
            active.sort();
            let primary =
                OwnedObjectPath::try_from(field(&values, "PrimaryConnection")?.try_clone()?)?
                    .to_string();
            ensure!(
                dbus.get_name_owner(NM.try_into()?).await?.as_str() == owner,
                "NetworkManager restarted during observation"
            );
            Ok(Observation {
                network: Some(Network {
                    owner,
                    active,
                    primary,
                }),
                sleeping,
            })
        })
        .await
        .context("network observation timed out")?
    }
    async fn timezone(&self) -> Result<String> {
        tokio::time::timeout(IO, async {
            Ok(
                proxy(&self.connection, TIME, "/org/freedesktop/timedate1", TIME)
                    .await?
                    .get_property("Timezone")
                    .await?,
            )
        })
        .await
        .context("timezone observation timed out")?
    }
    async fn set_timezone(&self, zone: &str) -> Result<()> {
        tokio::time::timeout(IO, async {
            proxy(&self.connection, TIME, "/org/freedesktop/timedate1", TIME)
                .await?
                .call::<_, _, ()>("SetTimezone", &(zone, false))
                .await?;
            Ok(())
        })
        .await
        .context("timezone application reply lost")?
    }
    async fn wifi(&self, started: f64) -> Result<Fix> {
        // A private bus connection scopes even a GetClient whose reply is lost.
        // Dropping this attempt never leaves a client tied to the long-lived bus.
        let connection = tokio::time::timeout(
            IO,
            zbus::connection::Builder::system()?
                .method_timeout(IO)
                .build(),
        )
        .await??;
        wifi_on(&connection, started).await
    }
    async fn ip(&self) -> http::IpResult {
        http::lookup(&self.http).await
    }
}

struct GeoClient {
    connection: Connection,
    path: String,
}
impl Drop for GeoClient {
    fn drop(&mut self) {
        let connection = self.connection.clone();
        let path = self.path.clone();
        // Best effort, bounded cleanup on success, timeout, reload or cancellation.
        // Closing the dedicated connection is the final GeoClue ownership fence.
        tokio::spawn(async move {
            let _ = tokio::time::timeout(IO, async {
                let manager = proxy(
                    &connection,
                    GEO,
                    GEO_MANAGER,
                    "org.freedesktop.GeoClue2.Manager",
                )
                .await?;
                manager
                    .call::<_, _, ()>("DeleteClient", &(OwnedObjectPath::try_from(path)?,))
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
            .await;
        });
    }
}
pub(super) async fn wifi_on(connection: &Connection, started: f64) -> Result<Fix> {
    let manager = proxy(
        connection,
        GEO,
        GEO_MANAGER,
        "org.freedesktop.GeoClue2.Manager",
    )
    .await?;
    let path: OwnedObjectPath = manager.call("GetClient", &()).await?;
    let _lease = GeoClient {
        connection: connection.clone(),
        path: path.to_string(),
    };
    let client = proxy(
        connection,
        GEO,
        path.as_str(),
        "org.freedesktop.GeoClue2.Client",
    )
    .await?;
    client
        .set_property("DesktopId", "location-timezone")
        .await?;
    client.set_property("RequestedAccuracyLevel", 4_u32).await?;
    let mut updates = client.receive_signal("LocationUpdated").await?;
    client.call::<_, _, ()>("Start", &()).await?;
    let mut location: OwnedObjectPath = client.get_property("Location").await?;
    loop {
        if location.as_str() != "/" {
            let values = properties(
                connection,
                GEO,
                location.as_str(),
                "org.freedesktop.GeoClue2.Location",
            )
            .await?;
            let (seconds, micros) =
                <(u64, u64)>::try_from(field(&values, "Timestamp")?.try_clone()?)?;
            let fix = Fix {
                latitude: f64::try_from(field(&values, "Latitude")?)?,
                longitude: f64::try_from(field(&values, "Longitude")?)?,
                accuracy: f64::try_from(field(&values, "Accuracy")?)?,
                source: <&str>::try_from(field(&values, "Description")?)?.into(),
                timestamp: seconds as f64 + micros as f64 / 1_000_000.0,
                timezone: String::new(),
                country_code: String::new(),
            };
            if micros < 1_000_000 && fix.usable_wifi(started, wall_time()) {
                return Ok(fix);
            }
            tracing::debug!(source = %fix.source, accuracy_meters = fix.accuracy,
                age_seconds = wall_time() - fix.timestamp, predates_attempt = fix.timestamp < started,
                coordinates_valid = fix.coordinates_valid(), timestamp_micros_valid = micros < 1_000_000,
                "ignoring unusable GeoClue fix");
        }
        let Some(message) = updates.next().await else {
            bail!("GeoClue stream ended");
        };
        let (_, next): (OwnedObjectPath, OwnedObjectPath) = message.body().deserialize()?;
        location = next;
    }
}

pub(super) async fn events(
    connection: &Connection,
    changes: tokio::sync::watch::Sender<Control>,
) -> Result<()> {
    let login = proxy(
        connection,
        LOGIN,
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut sleeps = login.receive_signal("PrepareForSleep").await?;
    let mut reload = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    loop {
        tokio::select! {
            signal = sleeps.next() => {
                let message = signal.context("logind signal connection ended")?;
                let (sleeping,): (bool,) = message.body().deserialize()?;
                changes.send_modify(|s| { s.sleeping = sleeping; s.epoch = s.epoch.wrapping_add(1); });
            }
            _ = reload.recv() => changes.send_modify(|s| { s.reload = s.reload.wrapping_add(1); s.epoch = s.epoch.wrapping_add(1); }),
            _ = term.recv() => { changes.send_modify(|s| s.shutdown = true); return Ok(()); }
            _ = interrupt.recv() => { changes.send_modify(|s| s.shutdown = true); return Ok(()); }
        }
    }
}
