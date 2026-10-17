//! Fresh NetworkManager observations -> Wi-Fi-only beaconDB lookup.
//! No SSIDs or BSSIDs escape through diagnostics, and no IP fallback is allowed.
use super::{
    bus::{NM, field, properties, proxy},
    http,
    policy::Fix,
    wall_time,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
use zbus::Connection;
use zvariant::OwnedObjectPath;

const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ENDPOINT: &str = "https://api.beacondb.net/v1/geolocate";
const MAX_APS: usize = 100;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccessPoint {
    mac_address: String,
    signal_strength: i16,
    age: u64,
    #[serde(skip)]
    last_seen_ms: i64,
}

pub(super) struct Scan {
    points: Vec<AccessPoint>,
    // Conservative lower bound on observation time, NOT HTTP response time.
    observed_at: f64,
}

// NetworkManager LastScan and LastSeen use CLOCK_BOOTTIME, including suspend.
// /proc/uptime exposes that clock without a new native dependency.
pub(super) fn boottime_ms() -> Result<i64> {
    let uptime = std::fs::read_to_string("/proc/uptime")?;
    let seconds: f64 = uptime
        .split_whitespace()
        .next()
        .context("missing boot clock")?
        .parse()?;
    ensure!(seconds.is_finite() && seconds >= 0.0, "invalid boot clock");
    Ok((seconds * 1000.0) as i64)
}

fn eligible_address(address: &str) -> Option<String> {
    let parts: Vec<_> = address.split(':').collect();
    if parts.len() != 6
        || parts
            .iter()
            .any(|part| part.len() != 2 || !part.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    let octets: Vec<_> = parts
        .iter()
        .map(|p| u8::from_str_radix(p, 16).unwrap())
        .collect();
    // Both group/multicast and locally administered addresses are unsuitable.
    if octets[0] & 3 != 0 || octets.iter().all(|b| *b == 0) {
        return None;
    }
    Some(address.to_ascii_lowercase())
}

fn point(
    address: &str,
    ssid: &[u8],
    strength: u8,
    last_seen: i32,
    requested_ms: i64,
    now_ms: i64,
) -> Option<AccessPoint> {
    let mac_address = eligible_address(address)?;
    if ssid.is_empty()
        || ssid.len() > 32
        || ssid.ends_with(b"_nomap")
        || strength > 100
        || last_seen < 0
    {
        return None;
    }
    let last_seen_ms = i64::from(last_seen) * 1000;
    // LastSeen has whole-second resolution. Fail closed on the partial first
    // second rather than accepting an AP last observed before this request.
    if last_seen_ms < requested_ms || last_seen_ms > now_ms || now_ms - last_seen_ms > 60_000 {
        return None;
    }
    // Invert NM's rough -100..-40 dBm -> 0..100 quality conversion. It is an
    // estimate, not raw RSSI; retain the old GeoClue noise cutoff of -90 dBm.
    let signal_strength = -100 + (i16::from(strength) * 60 + 50) / 100;
    if signal_strength <= -90 {
        return None;
    }
    Some(AccessPoint {
        mac_address,
        signal_strength,
        age: (now_ms - last_seen_ms) as u64,
        last_seen_ms,
    })
}

async fn device_scan(connection: &Connection, path: &str) -> Result<Scan> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let wireless = proxy(connection, NM, path, WIRELESS).await?;
        let previous: i64 = wireless.get_property("LastScan").await?;
        let observed_at = wall_time();
        let requested_ms = boottime_ms()?;
        let options = BTreeMap::<&str, zvariant::Value<'_>>::new();
        wireless
            .call::<_, _, ()>("RequestScan", &(options,))
            .await?;
        loop {
            let last: i64 = wireless.get_property("LastScan").await?;
            let now = boottime_ms()?;
            if last > previous && last >= requested_ms && last <= now {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let paths: Vec<OwnedObjectPath> = wireless.call("GetAllAccessPoints", &()).await?;
        ensure!(paths.len() <= 1024, "too many access point objects");
        let mut points = Vec::new();
        for path in paths {
            // APs may disappear mid-snapshot. Never substitute cached observations.
            let Ok(values) = properties(connection, NM, path.as_str(), AP).await else {
                continue;
            };
            let parsed = (|| -> Result<Option<AccessPoint>> {
                Ok(point(
                    <&str>::try_from(field(&values, "HwAddress")?)?,
                    &Vec::<u8>::try_from(field(&values, "Ssid")?.try_clone()?)?,
                    u8::try_from(field(&values, "Strength")?)?,
                    i32::try_from(field(&values, "LastSeen")?)?,
                    requested_ms,
                    boottime_ms()?,
                ))
            })();
            if let Ok(Some(point)) = parsed {
                points.push(point);
            }
        }
        Ok(Scan {
            points,
            observed_at,
        })
    })
    .await
    .context("fresh Wi-Fi scan timed out")?
}

pub(super) async fn scan(connection: &Connection) -> Result<Scan> {
    tokio::time::timeout(Duration::from_secs(50), async {
        let dbus = zbus::fdo::DBusProxy::new(connection).await?;
        let owner = dbus.get_name_owner(NM.try_into()?).await?;
        let manager = proxy(connection, NM, "/org/freedesktop/NetworkManager", NM).await?;
        let devices: Vec<OwnedObjectPath> = manager.call("GetDevices", &()).await?;
        ensure!(devices.len() <= 64, "too many network devices");
        let mut wireless = Vec::new();
        for path in devices {
            let values = properties(connection, NM, path.as_str(), DEVICE).await?;
            if u32::try_from(field(&values, "DeviceType")?)? == 2
                && bool::try_from(field(&values, "Managed")?)?
            {
                wireless.push(path);
            }
        }
        ensure!(
            !wireless.is_empty() && wireless.len() <= 8,
            "no usable Wi-Fi devices"
        );
        let scans =
            futures::future::join_all(wireless.iter().map(|p| device_scan(connection, p.as_str())))
                .await;
        ensure!(
            dbus.get_name_owner(NM.try_into()?).await? == owner,
            "NetworkManager changed during scan"
        );
        let mut points: BTreeMap<String, AccessPoint> = BTreeMap::new();
        let mut observed_at = wall_time();
        for result in scans {
            match result {
                Ok(scan) => {
                    if !scan.points.is_empty() {
                        observed_at = observed_at.min(scan.observed_at);
                    }
                    for ap in scan.points {
                        let existing = points.entry(ap.mac_address.clone()).or_insert(ap.clone());
                        if (ap.last_seen_ms, ap.signal_strength)
                            > (existing.last_seen_ms, existing.signal_strength)
                        {
                            *existing = ap;
                        }
                    }
                }
                Err(_) => tracing::debug!("Wi-Fi device scan unavailable"),
            }
        }
        let mut points: Vec<_> = points.into_values().collect();
        points.sort_by(|a, b| {
            b.signal_strength
                .cmp(&a.signal_strength)
                .then(a.mac_address.cmp(&b.mac_address))
        });
        points.truncate(MAX_APS);
        tracing::info!(
            eligible_access_points = points.len(),
            "Filtered fresh Wi-Fi scan completed"
        );
        ensure!(
            points.len() >= 2,
            "fewer than two eligible fresh Wi-Fi access points"
        );
        Ok(Scan {
            points,
            observed_at,
        })
    })
    .await
    .context("Wi-Fi acquisition timed out")?
}

pub(super) async fn lookup(
    client: &reqwest::Client,
    scan: Scan,
    started: f64,
) -> Result<http::LookupResult> {
    fetch(client, ENDPOINT, scan, started).await
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    mut scan: Scan,
    started: f64,
) -> Result<http::LookupResult> {
    let now_ms = boottime_ms()?;
    scan.points.retain_mut(|ap| {
        let age = now_ms - ap.last_seen_ms;
        if !(0..=60_000).contains(&age) {
            return false;
        }
        ap.age = age as u64;
        true
    });
    ensure!(
        scan.points.len() >= 2,
        "insufficient fresh Wi-Fi observations"
    );
    ensure!(
        scan.observed_at >= started && wall_time() - scan.observed_at <= 90.0,
        "Wi-Fi scan predates attempt"
    );
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "considerIp": false,
            "fallbacks": {"ipf": false, "lacf": false},
            "wifiAccessPoints": scan.points,
        }))
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    match http::response_body(response, "beaconDB").await? {
        http::Body::Limited(delay) => Ok(http::LookupResult::Limited(delay)),
        http::Body::Bytes(bytes) => {
            let fix = parse(&bytes, scan.observed_at, started)?;
            tracing::info!(
                scan_age_seconds = wall_time() - scan.observed_at,
                "Wi-Fi-only provider lookup completed"
            );
            Ok(http::LookupResult::Fix(fix))
        }
    }
}

fn parse(bytes: &[u8], observed_at: f64, started: f64) -> Result<Fix> {
    #[derive(Deserialize)]
    struct Coordinates {
        lat: f64,
        lng: f64,
    }
    #[derive(Deserialize)]
    struct Payload {
        location: Coordinates,
        accuracy: f64,
        fallback: Option<serde_json::Value>,
        error: Option<serde_json::Value>,
    }
    let payload: Payload = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid Wi-Fi response JSON"))?;
    ensure!(
        payload.fallback.is_none() && payload.error.is_none(),
        "Wi-Fi fallback or error response rejected"
    );
    let fix = Fix {
        latitude: payload.location.lat,
        longitude: payload.location.lng,
        accuracy: payload.accuracy,
        timestamp: observed_at,
        source: "WiFi".into(),
        timezone: String::new(),
        country_code: String::new(),
    };
    ensure!(
        fix.usable_wifi(started, wall_time()),
        "invalid Wi-Fi location evidence"
    );
    Ok(fix)
}

#[cfg(test)]
mod tests;
