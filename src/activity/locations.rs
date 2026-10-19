//! Canonical location identity/merging belongs to the producer. Consumers own
//! locale sorting, filtering and selection, not cross-source record synthesis.
use super::model::{ActivityState, WeatherState, WorldClockState};
use crate::model::{LocationSource, TimezoneState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    // One stable automatic row, independent of saved weather/home locations.
    // The authority supplies accepted GeoClue coordinates (possibly coarse IP
    // estimates), not verified physical position. No configuration is rewritten.
    if let Some(fix) = &local.accepted_location {
        let (clock, clock_unavailable) = clock_at(&fix.timezone, "Current location", instant);
        locations.push(Location {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AcceptedLocation;

    #[test]
    fn city_clocks_do_not_depend_on_forecast_availability_or_provider_offsets() {
        let instant = "2026-10-19T12:26:00Z".parse::<DateTime<Utc>>().unwrap();
        let cities = [
            ("Amsterdam", "Europe/Amsterdam", 7200),
            ("Dublin", "Europe/Dublin", 3600),
            ("Hangzhou", "Asia/Shanghai", 28800),
            ("Oklahoma City", "America/Chicago", -18000),
            ("Taipei", "Asia/Taipei", 28800),
        ];
        for available in [false, true] {
            let activity = ActivityState {
                weather_locations: cities
                    .iter()
                    .map(|(name, zone, _)| WeatherState {
                        id: (*name).into(),
                        location: (*name).into(),
                        timezone: (*zone).into(),
                        available,
                        utc_offset_seconds: 0,
                        timezone_region_ids: vec!["stale-provider-region".into()],
                        error: (!available).then(|| "Forecast unavailable".into()),
                        ..Default::default()
                    })
                    .collect(),
                // Matching saved clocks must not override current timezone data either.
                world_clocks: vec![WorldClockState {
                    timezone: "Europe/Amsterdam".into(),
                    label: "Saved name".into(),
                    utc_offset_seconds: -999,
                    abbreviation: "stale".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let rows = project_at(&activity, &TimezoneState::default(), instant);
            assert_eq!(rows.len(), cities.len());
            for (name, zone, offset) in cities {
                let row = rows
                    .iter()
                    .find(|r| r.id == format!("weather:{name}"))
                    .unwrap();
                let expected = WorldClockState::at(zone, name, instant).unwrap();
                assert_eq!(row.clock, expected);
                assert_eq!(row.clock.utc_offset_seconds, offset);
                assert!(!row.clock_unavailable);
                assert_eq!(row.weather.as_ref().unwrap().available, available);
            }
        }
    }

    #[test]
    fn cached_forecasts_cannot_freeze_dst_and_invalid_zones_are_not_utc() {
        let mut activity = ActivityState {
            weather_locations: vec![WeatherState {
                id: "home".into(),
                location: "Amsterdam".into(),
                timezone: "Europe/Amsterdam".into(),
                available: true,
                utc_offset_seconds: 7200,
                ..Default::default()
            }],
            ..Default::default()
        };
        for (date, offset) in [
            ("2026-10-25T00:59:59Z", 7200),
            ("2026-10-25T01:00:00Z", 3600),
        ] {
            let rows = project_at(&activity, &TimezoneState::default(), date.parse().unwrap());
            assert_eq!(rows[0].clock.utc_offset_seconds, offset);
            assert!(!rows[0].clock_unavailable);
        }
        let instant = "2026-10-19T12:26:00Z".parse().unwrap();
        for zone in ["", "Invalid/Zone", "Etc/UTC"] {
            activity.weather_locations[0].timezone = zone.into();
            let rows = project_at(&activity, &TimezoneState::default(), instant);
            assert_eq!(rows[0].id, "weather:home");
            assert_eq!(rows[0].clock.label, "Amsterdam");
            assert_eq!(rows[0].clock_unavailable, zone != "Etc/UTC");
            if rows[0].clock_unavailable {
                assert!(rows[0].clock.abbreviation.is_empty());
                assert!(rows[0].clock.timezone_region_ids.is_empty());
                assert_eq!(
                    serde_json::to_value(&rows[0]).unwrap()["clock_unavailable"],
                    true
                );
            } else {
                assert_eq!(rows[0].clock.utc_offset_seconds, 0);
                assert_eq!(rows[0].clock.abbreviation, "UTC");
            }
        }
    }

    #[test]
    fn estimated_metadata_keeps_current_identity_without_changing_saved_locations() {
        let mut local = TimezoneState {
            accepted_location: Some(AcceptedLocation {
                timezone: "Europe/Dublin".into(),
                latitude: 53.3,
                longitude: -6.2,
                verified_at: 1000.0,
                source: LocationSource::Ip,
                accuracy_meters: Some(26_000.0),
            }),
            ..Default::default()
        };
        let mut activity = ActivityState::default();
        activity
            .world_clocks
            .push(WorldClockState::new("Europe/Dublin", "Saved clock").unwrap());
        let rows = project(&activity, &local);
        let current = rows.iter().find(|row| row.current).unwrap();
        assert_eq!(current.id, "current-location");
        assert_eq!(current.clock.label, "Current location");
        assert_eq!(current.location_source, Some(LocationSource::Ip));
        assert_eq!(current.accuracy_meters, Some(26_000.0));
        assert!(!current.home && !current.has_weather);
        let saved = rows.iter().find(|row| !row.current).unwrap();
        assert_eq!(saved.clock.label, "Saved clock");
        assert_eq!(saved.location_source, None);
        local.accepted_location.as_mut().unwrap().source = LocationSource::Unknown;
        local.accepted_location.as_mut().unwrap().accuracy_meters = None;
        let rows = project(&activity, &local);
        let current = rows.iter().find(|row| row.current).unwrap();
        assert_eq!(current.id, "current-location");
        assert_eq!(current.location_source, Some(LocationSource::Unknown));
        assert_eq!(current.accuracy_meters, None);
    }
}
