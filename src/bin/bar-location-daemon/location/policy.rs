//! Fail-closed evidence policy and connection-scoped retry scheduling.
use std::{collections::HashMap, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Fix {
    pub latitude: f64,
    pub longitude: f64,
    pub timestamp: f64,
    pub source: String,
    #[serde(default)]
    pub accuracy: f64,
    #[serde(default)]
    pub timezone: String,
    #[serde(default)]
    pub country_code: String,
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
            && self.timestamp >= started
            && self.timestamp <= now + 5.0
            && now - self.timestamp <= 300.0
    }
    pub fn usable_wifi(&self, started: f64, now: f64) -> bool {
        self.fresh(started, now)
            && self.source == "WiFi"
            && self.accuracy.is_finite()
            && self.accuracy > 0.0
            && self.accuracy <= 25_000.0
    }
}

pub(super) struct Zones {
    finder: tzf_rs::DefaultFinder,
    links: HashMap<String, String>,
    countries: HashMap<String, String>,
}
impl Zones {
    pub fn load(directory: &Path) -> Result<Self> {
        Self::from_tables(
            &std::fs::read_to_string(directory.join("tzdata.zi"))?,
            &std::fs::read_to_string(directory.join("zone.tab"))?,
        )
    }
    pub fn from_tables(links: &str, countries: &str) -> Result<Self> {
        let mut result = Self {
            finder: tzf_rs::DefaultFinder::new(),
            links: HashMap::new(),
            countries: HashMap::new(),
        };
        for line in links.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if matches!(fields.first(), Some(&"L" | &"Link")) && fields.len() >= 3 {
                result.links.insert(fields[2].into(), fields[1].into());
            }
        }
        for line in countries.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() >= 3 {
                let zone = result.canonical(fields[2]).context("unknown tzdata zone")?;
                // zone.tab lists country-specific zones; aliases can share a target.
                result
                    .countries
                    .entry(zone)
                    .or_default()
                    .push_str(&format!(",{},", fields[0]));
            }
        }
        ensure!(
            !result.countries.is_empty(),
            "tzdata country table is empty"
        );
        Ok(result)
    }
    pub fn canonical(&self, name: &str) -> Option<String> {
        let mut name = name;
        for _ in 0..20 {
            match self.links.get(name) {
                Some(target) => name = target,
                None => return name.parse::<chrono_tz::Tz>().ok().map(|_| name.to_owned()),
            }
        }
        None
    }
    fn at(&self, fix: &Fix) -> Option<String> {
        let name = self.canonical(self.finder.get_tz_name(fix.longitude, fix.latitude))?;
        // Bundled TZF boundaries are simplified (~111m tolerance). Reject near
        // boundaries rather than pretending this is the old exact land dataset.
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
    pub fn agree(&self, wifi: &Fix, ip: &Fix, started: f64, now: f64) -> Result<String, Decision> {
        if !wifi.usable_wifi(started, now)
            || !ip.fresh(started, now)
            || (wifi.timestamp - ip.timestamp).abs() > 120.0
        {
            return Err(Decision::Retry("Fresh location unavailable"));
        }
        let zone = self
            .at(wifi)
            .ok_or(Decision::Stop("Timezone boundary uncertain"))?;
        if distance_km(wifi, ip) > 50.0
            || ip.source != "IP"
            || self.at(ip).as_deref() != Some(&zone)
            || self.canonical(&ip.timezone).as_deref() != Some(&zone)
        {
            return Err(Decision::Stop("Sources disagree"));
        }
        if ip.country_code.len() != 2
            || !ip.country_code.bytes().all(|c| c.is_ascii_alphabetic())
            || !self.countries.get(&zone).is_some_and(|countries| {
                countries.contains(&format!(",{},", ip.country_code.to_ascii_uppercase()))
            })
        {
            return Err(Decision::Stop("IP country and timezone disagree"));
        }
        Ok(zone)
    }
}
fn distance_km(a: &Fix, b: &Fix) -> f64 {
    let dlat = (b.latitude - a.latitude).to_radians();
    let dlon = (b.longitude - a.longitude).to_radians();
    let h = (dlat / 2.0).sin().powi(2)
        + a.latitude.to_radians().cos()
            * b.latitude.to_radians().cos()
            * (dlon / 2.0).sin().powi(2);
    6371.0 * 2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

#[derive(Debug, PartialEq)]
pub(super) enum Decision {
    Retry(&'static str),
    Stop(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Network {
    pub owner: String,
    pub active: Vec<String>,
    pub primary: String,
}

#[derive(Default)]
pub(super) struct Schedule {
    pub network: Option<Network>,
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
            self.retry_network = network.clone();
            self.retry_due = None;
            self.retries = 0;
        }
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
