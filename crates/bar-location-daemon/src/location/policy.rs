//! GeoClue first; only canonical timezone equality is required for confirmation.
use std::{path::Path, time::Duration};

use crate::location_place::TimezoneLinks;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Source {
    Ip,
    NonIp,
    Manual,
    #[default]
    #[serde(other)]
    Unknown,
}
impl Source {
    /// Shelllist's additive GeoClue SourceKind property, not Description.
    pub fn from_geoclue(value: u32) -> Self {
        match value {
            1 => Self::Ip,
            2 => Self::NonIp,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Fix {
    pub latitude: f64,
    pub longitude: f64,
    pub timestamp: f64,
    pub source: Source,
    pub accuracy: f64,
}
impl Fix {
    pub fn coordinates_valid(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && self.latitude.abs() <= 90.0
            && self.longitude.abs() <= 180.0
    }
    pub fn fresh(&self, started: f64, now: f64) -> bool {
        self.coordinates_valid()
            && self.timestamp.is_finite()
            // `started` is the current network epoch, not each retry's start.
            // GeoClue 2.7 timestamps have whole-second precision.
            && self.timestamp >= started.floor()
            && self.timestamp <= now + 5.0
            && now - self.timestamp <= 300.0
    }
    pub fn usable_geoclue(&self, started: f64, now: f64) -> bool {
        self.fresh(started, now)
            // Leave room for the bounded IP request within the 120s pair window.
            && (self.source == Source::Ip || now - self.timestamp <= 90.0)
            && self.accuracy.is_finite() && self.accuracy >= 0.0
    }
}

pub(super) struct Zones {
    finder: tzf_rs::DefaultFinder,
    links: TimezoneLinks,
}
impl Zones {
    pub fn load(directory: &Path) -> Result<Self> {
        Ok(Self::from_table(&std::fs::read_to_string(
            directory.join("tzdata.zi"),
        )?))
    }
    pub fn from_table(links: &str) -> Self {
        Self {
            finder: tzf_rs::DefaultFinder::new(),
            links: TimezoneLinks::from_table(links),
        }
    }
    pub fn canonical(&self, name: &str) -> Option<String> {
        let name = self.links.resolve(name)?;
        name.parse::<chrono_tz::Tz>().ok().map(|_| name.to_owned())
    }
    pub(super) fn at(&self, fix: &Fix) -> Option<String> {
        let name = self.canonical(self.finder.get_tz_name(fix.longitude, fix.latitude))?;
        // The bundled boundaries are simplified (~111m tolerance). Ambiguity is
        // missing evidence (retry), not a confirmed disagreement (stop).
        for dx in [-0.002, 0.0, 0.002] {
            for dy in [-0.002, 0.0, 0.002] {
                let names = self
                    .finder
                    .get_tz_names(fix.longitude + dx, fix.latitude + dy);
                if names.len() != 1 || self.canonical(names[0]).as_deref() != Some(&name) {
                    return None;
                }
            }
        }
        Some(name)
    }
    pub fn geoclue_zone(&self, geo: &Fix, started: f64, now: f64) -> Result<String, Decision> {
        if !geo.usable_geoclue(started, now) {
            return Err(Decision::Retry("Fresh GeoClue location unavailable"));
        }
        self.at(geo)
            .ok_or(Decision::Retry("GeoClue timezone unresolved"))
    }
    pub fn decide(
        &self,
        geo: &Fix,
        ip: Option<&Fix>,
        started: f64,
        now: f64,
    ) -> Result<String, Decision> {
        let zone = self.geoclue_zone(geo, started, now)?;
        if geo.source == Source::Ip {
            tracing::info!(timezone = %zone, "accepting GeoClue IP location without a second lookup");
            return Ok(zone);
        }
        let ip = ip.ok_or(Decision::Retry("IP confirmation unavailable"))?;
        if !ip.fresh(started, now)
            || ip.source != Source::Ip
            || (geo.timestamp - ip.timestamp).abs() > 120.0
        {
            return Err(Decision::Retry("Fresh IP confirmation unavailable"));
        }
        let ip_zone = self
            .at(ip)
            .ok_or(Decision::Retry("IP timezone unresolved"))?;
        tracing::info!(source = ?geo.source, geoclue_timezone = %zone, ip_timezone = %ip_zone,
            "comparing canonical timezones (no distance requirement)");
        if zone != ip_zone {
            return Err(Decision::Stop("GeoClue and IP timezones disagree"));
        }
        Ok(zone)
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Decision {
    Retry(&'static str),
    IpConfirmationLimited,
    Stop(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Network {
    pub owner: String,
    pub active: Vec<String>,
    pub primary: String,
}

impl Network {
    pub fn changed_fields(&self, previous: &Self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.owner != previous.owner {
            fields.push("owner");
        }
        if self.active != previous.active {
            fields.push("active_connections");
        }
        if self.primary != previous.primary {
            fields.push("primary_connection");
        }
        fields
    }
}

#[derive(Default)]
pub(super) struct Schedule {
    pub network: Option<Network>,
    pub network_since: Option<f64>,
    completed: Option<Network>,
    retry_network: Option<Network>,
    retry_due: Option<Instant>,
    retries: u32,
    pub due: Option<Instant>,
}
impl Schedule {
    pub fn observe(&mut self, network: Option<Network>, now: Instant) -> bool {
        if self.network == network {
            return false;
        }
        if network.is_some() && self.retry_network != network {
            if let (Some(current), Some(previous)) = (&network, &self.retry_network) {
                // Field names only: never emit identifiers, addresses or raw objects.
                tracing::info!(changed_fields = ?current.changed_fields(previous), "network identity changed");
            } else {
                tracing::info!("first eligible network observed");
            }
            self.retry_network = network.clone();
            self.retry_due = None;
            self.retries = 0;
        }
        // A reconnect/resume is a new freshness epoch even when identity recurs.
        // Retrying or explicitly reloading on the same observed network is not.
        self.network_since = network.as_ref().map(|_| super::wall_time());
        self.due = network
            .as_ref()
            .filter(|n| Some(*n) != self.completed.as_ref())
            .map(|_| {
                self.retry_due
                    .unwrap_or(now)
                    .max(now + Duration::from_secs(10))
            });
        self.network = network;
        true
    }
    pub fn rearm(&mut self) {
        self.completed = None;
        self.retry_due = None;
        self.retries = 0;
        self.due = self
            .network
            .as_ref()
            .map(|_| Instant::now() + Duration::from_secs(10));
    }
    pub fn ready(&self, now: Instant) -> bool {
        self.network.is_some() && self.due.is_some_and(|due| due <= now)
    }
    pub fn complete(&mut self) {
        self.completed = self.network.clone();
        self.due = None;
    }
    pub fn retry(&mut self, now: Instant, minimum: Duration) {
        let delay = Duration::from_secs((30 * 2_u64.pow(self.retries)).min(300)).max(minimum);
        self.retries = (self.retries + 1).min(4);
        self.retry_due = Some(now + delay);
        self.due = self.network.as_ref().map(|_| now + delay);
    }
}
