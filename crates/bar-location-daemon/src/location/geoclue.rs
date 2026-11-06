//! Dedicated GeoClue client per attempt. Provenance comes from SourceKind,
//! an additive property supplied by packaging/geoclue, never Description.
use super::{
    bus::{field, properties, proxy},
    policy::{Fix, Source},
    wall_time,
};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::time::Duration;
use zbus::{Connection, Proxy};
use zvariant::OwnedObjectPath;

const GEO: &str = "org.freedesktop.GeoClue2";
const MANAGER: &str = "/org/freedesktop/GeoClue2/Manager";
const IO: Duration = Duration::from_secs(5);

fn revalidation_cutoff(network_since: f64, now: f64) -> u64 {
    // Round the age cutoff up, not down, and leave response-time headroom
    // inside the 90s non-IP usability window. Never return the just-expired hit.
    network_since.floor().max((now - 60.0).ceil()).max(0.0) as u64
}

pub(super) async fn locate(started: f64) -> Result<Fix> {
    // Dropping the dedicated connection scopes even a GetClient with a lost reply.
    let connection = tokio::time::timeout(
        IO,
        zbus::connection::Builder::system()?
            .method_timeout(IO)
            .build(),
    )
    .await??;
    locate_on(&connection, started).await
}

struct ClientLease {
    manager: Proxy<'static>,
    path: OwnedObjectPath,
}
impl Drop for ClientLease {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let path = self.path.clone();
        // Only cleanup is detached. The retained unique-owner proxy cannot hit
        // a replacement daemon; its connection is released even on failure.
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(IO, manager.call::<_, _, ()>("DeleteClient", &(path,))).await;
        });
    }
}

pub(super) async fn locate_on(connection: &Connection, started: f64) -> Result<Fix> {
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let owner = match dbus.get_name_owner(GEO.try_into()?).await {
        Ok(owner) => owner,
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
            dbus.start_service_by_name(GEO.try_into()?, 0).await?;
            dbus.get_name_owner(GEO.try_into()?).await?
        }
        Err(error) => return Err(error.into()),
    };
    let manager = Proxy::new_owned(
        connection.clone(),
        owner,
        MANAGER,
        "org.freedesktop.GeoClue2.Manager",
    )
    .await?;
    let path = manager.call("GetClient", &()).await?;
    let lease = ClientLease { manager, path };
    let owner = lease.manager.destination().as_str();
    let client = proxy(
        connection,
        owner,
        lease.path.as_str(),
        "org.freedesktop.GeoClue2.Client",
    )
    .await?;
    client
        .set_property("DesktopId", "location-timezone")
        .await?;
    client.set_property("RequestedAccuracyLevel", 4_u32).await?;
    let fix = wait_for_fix(&client, started).await?;
    // A replacement GeoClue must not make old objects authoritative.
    anyhow::ensure!(
        dbus.get_name_owner(GEO.try_into()?).await?.as_str() == owner,
        "GeoClue restarted during acquisition"
    );
    Ok(fix)
}

async fn wait_for_fix(client: &Proxy<'_>, started: f64) -> Result<Fix> {
    let mut updates = client.receive_signal("LocationUpdated").await?;
    client.call::<_, _, ()>("Start", &()).await?;
    let mut location: OwnedObjectPath = client.get_property("Location").await?;
    let mut refreshed = false;
    loop {
        if location.as_str() != "/" {
            if let Some(fix) = fresh_fix(client, &location, started).await? {
                return Ok(fix);
            }
            if !refreshed {
                refreshed = true;
                // Shelllist extension: revalidate stale shared sources once, rather
                // than relying on GeoClue's process exiting to clear its cache.
                // Old/unpatched services may not support it; remain conservative.
                let minimum = revalidation_cutoff(started, wall_time());
                if let Err(error) = client.call::<_, _, ()>("RefreshSince", &(minimum,)).await {
                    tracing::debug!(%error, "GeoClue revalidation unavailable");
                }
            }
        }
        let message = updates
            .next()
            .await
            .context("GeoClue update stream ended")?;
        let (_, next): (OwnedObjectPath, OwnedObjectPath) = message.body().deserialize()?;
        if next.as_str() == "/" {
            bail!("GeoClue location unavailable");
        }
        location = next;
    }
}

async fn fresh_fix(
    client: &Proxy<'_>,
    location: &OwnedObjectPath,
    started: f64,
) -> Result<Option<Fix>> {
    let mut values = properties(
        client.connection(),
        client.destination().as_str(),
        location.as_str(),
        "org.freedesktop.GeoClue2.Location",
    )
    .await?;
    let (seconds, micros) = <(u64, u64)>::try_from(
        values
            .remove("Timestamp")
            .context("missing D-Bus property Timestamp")?,
    )?;
    let fix = Fix {
        latitude: f64::try_from(field(&values, "Latitude")?)?,
        longitude: f64::try_from(field(&values, "Longitude")?)?,
        accuracy: f64::try_from(field(&values, "Accuracy")?)?,
        timestamp: seconds as f64 + micros as f64 / 1_000_000.0,
        source: values
            .get("SourceKind")
            .and_then(|v| u32::try_from(v).ok())
            .map(Source::from_geoclue)
            .unwrap_or_default(),
    };
    if micros < 1_000_000 && fix.usable_geoclue(started, wall_time()) {
        return Ok(Some(fix));
    }
    tracing::debug!(source = ?fix.source, "waiting for a usable fresh GeoClue fix");
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::revalidation_cutoff;

    #[test]
    fn cutoff_excludes_expired_whole_seconds_and_preserves_network_fence() {
        assert_eq!(revalidation_cutoff(800.0, 1000.9), 941);
        assert_eq!(revalidation_cutoff(950.9, 1000.9), 950);
        assert_eq!(revalidation_cutoff(0.0, 30.0), 0);
        assert!(revalidation_cutoff(800.0, 1000.9) > 910);
    }
}
