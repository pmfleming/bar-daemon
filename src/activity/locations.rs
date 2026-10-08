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
        clocks
            .entry(clock.timezone.clone())
            .or_insert(clock.clone());
    }
    let mut represented = BTreeSet::new();
    let mut locations = Vec::new();
    for weather in &activity.weather_locations {
        let mut clock = clocks.get(&weather.timezone).cloned().unwrap_or_default();
        clock.timezone = weather.timezone.clone();
        if !weather.location.is_empty() {
            clock.label = weather.location.clone();
        }
        if clock.city.is_empty() {
            clock.city = clock.label.clone();
        }
        clock.utc_offset_seconds = weather.utc_offset_seconds;
        if !weather.timezone_region_ids.is_empty() {
            clock.timezone_region_ids = weather.timezone_region_ids.clone();
        }
        let has_coordinates = weather.latitude.is_finite()
            && weather.longitude.is_finite()
            && (-90.0..=90.0).contains(&weather.latitude)
            && (-180.0..=180.0).contains(&weather.longitude);
        locations.push(Location {
            id: format!("weather:{}", weather.id),
            clock,
            latitude: if has_coordinates {
                weather.latitude
            } else {
                0.0
            },
            longitude: if has_coordinates {
                weather.longitude
            } else {
                0.0
            },
            has_coordinates,
            home: weather.home,
            has_weather: true,
            weather: Some(weather.clone()),
            lunar: activity.lunar.clone(),
        });
        represented.insert(weather.timezone.clone());
    }
    if local.available
        && !local.timezone.is_empty()
        && !represented.contains(&local.timezone)
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
            lunar: activity.lunar.clone(),
            ..Default::default()
        });
        represented.insert(local.timezone.clone());
    }
    for (timezone, clock) in clocks {
        if represented.insert(timezone.clone()) {
            locations.push(Location {
                id: format!("clock:{timezone}"),
                clock,
                lunar: activity.lunar.clone(),
                ..Default::default()
            });
        }
    }
    locations.sort_by(|a, b| a.id.cmp(&b.id));
    locations
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reordering_and_duplicate_clocks_do_not_change_identities() {
        let mut activity = ActivityState::default();
        activity.world_clocks = vec![
            WorldClockState::new("Etc/UTC", "Zulu").unwrap(),
            WorldClockState::new("Etc/UTC", "Alpha").unwrap(),
        ];
        let before = project(&activity, &Default::default());
        activity.world_clocks.reverse();
        assert_eq!(project(&activity, &Default::default()), before);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].id, "clock:Etc/UTC");
        assert_eq!(before[0].clock.label, "Alpha");
    }
    #[test]
    fn distinct_weather_places_in_one_timezone_survive_and_suppress_duplicate_clock() {
        let mut activity = ActivityState::default();
        activity
            .world_clocks
            .push(WorldClockState::new("Etc/UTC", "Clock").unwrap());
        for id in ["one", "two"] {
            activity.weather_locations.push(WeatherState {
                id: id.into(),
                timezone: "Etc/UTC".into(),
                location: id.into(),
                ..Default::default()
            });
        }
        let local = TimezoneState {
            available: true,
            timezone: "Etc/UTC".into(),
            ..Default::default()
        };
        let locations = project(&activity, &local);
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].id, "weather:one");
        assert_eq!(locations[1].id, "weather:two");
    }
}
