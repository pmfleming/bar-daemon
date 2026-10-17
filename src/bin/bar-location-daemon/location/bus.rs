use super::{Backend, Control, Observation, http, policy::Network, wifi};
use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use std::{collections::HashMap, time::Duration};
use zbus::{Connection, Proxy};
use zvariant::{OwnedObjectPath, OwnedValue};

pub(super) const NM: &str = "org.freedesktop.NetworkManager";
const TIME: &str = "org.freedesktop.timedate1";
const LOGIN: &str = "org.freedesktop.login1";
const IO: Duration = Duration::from_secs(5);

pub(super) struct Bus {
    pub connection: Connection,
    pub http: reqwest::Client,
}
pub(super) async fn proxy<'a>(
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
pub(super) async fn properties(
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
pub(super) fn field<'a>(
    values: &'a HashMap<String, OwnedValue>,
    name: &str,
) -> Result<&'a OwnedValue> {
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
    async fn wifi(&self, started: f64) -> Result<http::LookupResult> {
        let scan = wifi::scan(&self.connection).await?;
        wifi::lookup(&self.http, scan, started).await
    }
    async fn ip(&self) -> http::LookupResult {
        http::lookup(&self.http).await
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
