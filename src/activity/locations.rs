//! Canonical location identity/merging belongs to the producer. Consumers own
//! locale sorting, filtering and selection, not cross-source record synthesis.
use super::model::{ActivityState, WeatherState, WorldClockState};
use crate::model::TimezoneState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Location {
    pub id: String,
    #[serde(flatten)]
    pub clock: WorldClockState,
    pub latitude: f64,
    pub longitude: f64,
    pub has_coordinates: bool,
    pub home: bool,
    pub has_weather: bool,
    pub weather: Option<WeatherState>,
    pub lunar: Option<super::astronomy::LunarPhase>,
}

pub(crate) fn project(activity: &ActivityState, local: &TimezoneState) -> Vec<Location> {
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
        ));
        represented.insert(weather.timezone.as_str());
    }
    if local.available
        && !local.timezone.is_empty()
        && !represented.contains(local.timezone.as_str())
        && !locations.iter().any(|location| location.home)
    {
        locations.push(Location {
            id: format!("local:{}", local.timezone),
            clock: WorldClockState {
                timezone: local.timezone.clone(),
                label: local.city.clone(),
                city: local.city.clone(),
                abbreviation: local.abbreviation.clone(),
                utc_offset_seconds: local.utc_offset_seconds,
                timezone_region_ids: local.timezone_region_ids.clone(),
            },
            home: true,
            ..Default::default()
        });
        represented.insert(local.timezone.as_str());
    }
    for (timezone, clock) in clocks {
        if !represented.contains(timezone) {
            locations.push(Location {
                id: format!("clock:{timezone}"),
                clock: clock.clone(),
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

fn weather_location(weather: &WeatherState, clock: Option<&WorldClockState>) -> Location {
    let mut clock = clock.cloned().unwrap_or_default();
    clock.timezone.clone_from(&weather.timezone);
    if !weather.location.is_empty() {
        clock.label.clone_from(&weather.location);
    }
    if clock.city.is_empty() {
        clock.city.clone_from(&clock.label);
    }
    clock.utc_offset_seconds = weather.utc_offset_seconds;
    if !weather.timezone_region_ids.is_empty() {
        clock
            .timezone_region_ids
            .clone_from(&weather.timezone_region_ids);
    }
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
        latitude,
        longitude,
        has_coordinates,
        home: weather.home,
        has_weather: true,
        weather: Some(weather.clone()),
        ..Default::default()
    }
}
