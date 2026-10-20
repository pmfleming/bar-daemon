//! Canonical location identity/merging belongs to the producer. Consumers own
//! locale sorting, filtering and selection, not cross-source record synthesis.
use super::model::{ActivityState, WeatherState, WorldClockState};
use crate::{
    location_place::{Place, TimezoneLinks},
    model::{LocationSource, TimezoneState},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

// Catalog identity and the fixed Home city are session presentation policy,
// not part of the privileged authority's place validation.
pub(super) fn home_place() -> Place {
    Place {
        name: "Amsterdam".into(),
        country: "Netherlands".into(),
        region: "North Holland".into(),
        timezone: "Europe/Amsterdam".into(),
        latitude: 52.3676,
        longitude: 4.9041,
    }
}

pub(super) fn same_place(left: &Place, right: &Place) -> bool {
    left.name.eq_ignore_ascii_case(&right.name)
        && same_zone(&left.timezone, &right.timezone)
        && (left.latitude - right.latitude).abs() < 0.05
        && (left.longitude - right.longitude).abs() < 0.05
}

fn same_zone(left: &str, right: &str) -> bool {
    static LINKS: std::sync::OnceLock<TimezoneLinks> = std::sync::OnceLock::new();
    let links = LINKS.get_or_init(|| {
        let directory = std::env::var_os("TZDIR").map(std::path::PathBuf::from);
        let table = directory
            .iter()
            .map(|p| p.join("tzdata.zi"))
            .chain([
                Path::new("/etc/zoneinfo/tzdata.zi").to_owned(),
                Path::new("/usr/share/zoneinfo/tzdata.zi").to_owned(),
            ])
            .find_map(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        TimezoneLinks::from_table(&table)
    });
    left.parse::<chrono_tz::Tz>().is_ok()
        && right.parse::<chrono_tz::Tz>().is_ok()
        && matches!((links.resolve(left), links.resolve(right)), (Some(a), Some(b)) if a == b)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Location {
    pub id: String,
    #[serde(flatten)]
    pub clock: WorldClockState,
    /// Missing/invalid timezone: numeric defaults must not be presented as UTC.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clock_unavailable: bool,
    pub latitude: f64,
    pub longitude: f64,
    pub has_coordinates: bool,
    pub home: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<crate::location_place::Place>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_source: Option<LocationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy_meters: Option<f64>,
    pub has_weather: bool,
    pub weather: Option<WeatherState>,
    pub lunar: Option<super::astronomy::LunarPhase>,
}

pub(crate) fn project(activity: &ActivityState, local: &TimezoneState) -> Vec<Location> {
    project_at(activity, local, Utc::now())
}

pub(crate) fn project_at(
    activity: &ActivityState,
    local: &TimezoneState,
    instant: DateTime<Utc>,
) -> Vec<Location> {
    let mut clocks = BTreeMap::new();
    // Deterministic duplicate resolution, independent of source ordering.
    let mut ordered: Vec<_> = activity.world_clocks.iter().collect();
    ordered.sort_by(|a, b| (&a.timezone, &a.label).cmp(&(&b.timezone, &b.label)));
    for clock in ordered {
        clocks.entry(clock.timezone.as_str()).or_insert(clock);
    }
    let mut represented = BTreeSet::new();
    let mut locations = Vec::new();
    for weather in &activity.weather_locations {
        locations.push(weather_location(
            weather,
            clocks.get(weather.timezone.as_str()).copied(),
            instant,
        ));
        represented.insert(weather.timezone.as_str());
    }
    if local.accepted_location.is_some() {
        append_current(&mut locations, local, instant);
    }
    if local.accepted_location.is_none()
        && local.available
        && !local.timezone.is_empty()
        && !represented.contains(local.timezone.as_str())
        && !locations.iter().any(|location| location.home)
    {
        let (clock, clock_unavailable) = clock_at(&local.timezone, &local.city, instant);
        locations.push(Location {
            id: format!("local:{}", local.timezone),
            clock,
            clock_unavailable,
            home: true,
            ..Default::default()
        });
        represented.insert(local.timezone.as_str());
    }
    for (timezone, clock) in clocks {
        if !represented.contains(timezone) {
            let (clock, clock_unavailable) = clock_at(timezone, &clock.label, instant);
            locations.push(Location {
                id: format!("clock:{timezone}"),
                clock,
                clock_unavailable,
                ..Default::default()
            });
        }
    }
    for location in &mut locations {
        location.lunar = activity.lunar.clone();
    }
    locations.sort_by(|a, b| a.id.cmp(&b.id));
    locations
}

pub(crate) fn append_current(
    locations: &mut Vec<Location>,
    local: &TimezoneState,
    instant: DateTime<Utc>,
) {
    // Replacing the effective location never appends a second Current entry.
    locations.retain(|row| row.id != "current-location");
    for row in locations.iter_mut() {
        row.current = false;
    }
    let Some(fix) = &local.accepted_location else {
        return;
    };
    // At home there is no separate Current presentation, including on Home.
    // Unknown location likewise has no fabricated/blank placeholder.
    if same_zone(&fix.timezone, "Europe/Amsterdam") {
        return;
    }
    let (mut clock, clock_unavailable) = clock_at(&fix.timezone, "Current location", instant);
    if clock_unavailable
        || !(-90.0..=90.0).contains(&fix.latitude)
        || !(-180.0..=180.0).contains(&fix.longitude)
    {
        return;
    }
    let named_place = local
        .location_control
        .as_ref()
        .and_then(|c| c.effective_place.as_ref())
        .filter(|place| {
            place.validate().is_ok()
                && same_zone(&place.timezone, &fix.timezone)
                && (place.latitude - fix.latitude).abs() < 0.000001
                && (place.longitude - fix.longitude).abs() < 0.000001
        });
    let place = named_place.cloned().unwrap_or_else(|| Place {
        name: clock.city.clone(),
        country: String::new(),
        region: String::new(),
        timezone: fix.timezone.clone(),
        latitude: fix.latitude,
        longitude: fix.longitude,
    });
    if named_place.is_some() {
        clock.label = place.name.clone();
        clock.city = place.name.clone();
    }
    // Reuse a saved city's forecast when it is the same place, never another
    // city's weather just because it shares a timezone. No new provider I/O.
    let weather = locations
        .iter()
        .find(|row| {
            row.place
                .as_ref()
                .is_some_and(|saved| same_place(saved, &place))
        })
        .and_then(|row| row.weather.clone());
    locations.push(Location {
        has_weather: weather.is_some(),
        weather,
        place: Some(place),
        location_mode: local
            .location_control
            .as_ref()
            .map(|c| if c.manual { "manual" } else { "automatic" }.into()),
        id: "current-location".into(),
        clock,
        clock_unavailable,
        latitude: fix.latitude,
        longitude: fix.longitude,
        has_coordinates: true,
        current: true,
        location_source: Some(fix.source),
        accuracy_meters: fix.accuracy_meters,
        ..Default::default()
    });
}

fn clock_at(timezone: &str, label: &str, instant: DateTime<Utc>) -> (WorldClockState, bool) {
    match WorldClockState::at(timezone, label, instant) {
        Ok(clock) => (clock, false),
        Err(_) => (
            WorldClockState {
                timezone: timezone.into(),
                label: label.into(),
                city: label.into(),
                ..Default::default()
            },
            true,
        ),
    }
}

fn weather_location(
    weather: &WeatherState,
    saved: Option<&WorldClockState>,
    instant: DateTime<Utc>,
) -> Location {
    let label = if weather.location.is_empty() {
        saved.map(|clock| clock.label.as_str()).unwrap_or("")
    } else {
        &weather.location
    };
    // Clocks are local timezone calculations, not forecast telemetry. A failed
    // first fetch has a zero offset; even a successful cached forecast can carry
    // yesterday's DST offset. Never let either overwrite the current clock.
    let (clock, clock_unavailable) = clock_at(&weather.timezone, label, instant);
    // Range checks also reject NaN and infinities. Invalid pairs become (0, 0).
    let has_coordinates =
        (-90.0..=90.0).contains(&weather.latitude) && (-180.0..=180.0).contains(&weather.longitude);
    let (latitude, longitude) = if has_coordinates {
        (weather.latitude, weather.longitude)
    } else {
        (0.0, 0.0)
    };
    Location {
        id: format!("weather:{}", weather.id),
        clock,
        clock_unavailable,
        latitude,
        longitude,
        has_coordinates,
        home: weather.home,
        has_weather: true,
        weather: Some(weather.clone()),
        ..Default::default()
    }
}
