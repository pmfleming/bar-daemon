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
use zbus::Connection;
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
    connection: Connection,
    owner: String,
    path: OwnedObjectPath,
}
impl Drop for ClientLease {
    fn drop(&mut self) {
        let connection = self.connection.clone();
        let owner = self.owner.clone();
        let path = self.path.clone();
        // Only cleanup is detached; it cannot look up or apply a timezone.
        // The unique owner prevents cleanup hitting a replacement daemon.
        tokio::spawn(async move {
            let _ = tokio::time::timeout(IO, async {
                proxy(
                    &connection,
                    &owner,
                    MANAGER,
                    "org.freedesktop.GeoClue2.Manager",
                )
                .await?
                .call::<_, _, ()>("DeleteClient", &(path,))
                .await?;
                Ok::<_, anyhow::Error>(())
            })
            .await;
            // Last connection reference is released even if DeleteClient failed.
        });
    }
}

pub(super) async fn locate_on(connection: &Connection, started: f64) -> Result<Fix> {
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let owner = match dbus.get_name_owner(GEO.try_into()?).await {
        Ok(owner) => owner.to_string(),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
            dbus.start_service_by_name(GEO.try_into()?, 0).await?;
            dbus.get_name_owner(GEO.try_into()?).await?.to_string()
        }
        Err(error) => return Err(error.into()),
    };
    let manager = proxy(
        connection,
        &owner,
        MANAGER,
        "org.freedesktop.GeoClue2.Manager",
    )
    .await?;
    let path: OwnedObjectPath = manager.call("GetClient", &()).await?;
    let _lease = ClientLease {
        connection: connection.clone(),
        owner: owner.clone(),
        path: path.clone(),
    };
    let client = proxy(
        connection,
        &owner,
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
    let mut refreshed = false;
    loop {
        if location.as_str() != "/" {
            let values = properties(
                connection,
                &owner,
                location.as_str(),
                "org.freedesktop.GeoClue2.Location",
            )
            .await?;
            let (seconds, micros) =
                <(u64, u64)>::try_from(field(&values, "Timestamp")?.try_clone()?)?;
            let source = values
                .get("SourceKind")
                .and_then(|v| u32::try_from(v).ok())
                .map(Source::from_geoclue)
                .unwrap_or_default();
            let fix = Fix {
                latitude: f64::try_from(field(&values, "Latitude")?)?,
                longitude: f64::try_from(field(&values, "Longitude")?)?,
                accuracy: f64::try_from(field(&values, "Accuracy")?)?,
                timestamp: seconds as f64 + micros as f64 / 1_000_000.0,
                source,
            };
            if micros < 1_000_000 && fix.usable_geoclue(started, wall_time()) {
                // A replacement GeoClue must not make old objects authoritative.
                anyhow::ensure!(
                    dbus.get_name_owner(GEO.try_into()?).await?.as_str() == owner,
                    "GeoClue restarted during acquisition"
                );
                return Ok(fix);
            }
            if !refreshed {
                refreshed = true;
                // Shelllist extension: revalidate stale shared sources once, rather
                // than relying on GeoClue's process exiting to clear its cache.
                // Old/unpatched services may not support it; remain conservative.
                let minimum = revalidation_cutoff(started, wall_time());
                match client.call::<_, _, ()>("RefreshSince", &(minimum,)).await {
                    Ok(()) => {}
                    Err(error) => tracing::debug!(%error, "GeoClue revalidation unavailable"),
                }
            }
            tracing::debug!(source = ?source, "waiting for a usable fresh GeoClue fix");
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
