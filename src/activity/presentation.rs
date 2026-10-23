//! Weather units, clock labels and forecast selection are native projections.
//! QML owns asset maps and pixel geometry only.
use super::model::{ActivityState, WeatherState, WorldClockState};
use crate::presentation::{number, utc_offset};
use chrono::{DateTime, Timelike, Utc};
use serde_json::json;

fn local(ms: i64, zone: &str, format: &str) -> String {
    let Some(instant) = DateTime::from_timestamp_millis(ms).filter(|_| ms > 0) else {
        return "—".into();
    };
    zone.parse::<chrono_tz::Tz>()
        .ok()
        .map(|tz| instant.with_timezone(&tz).format(format).to_string())
        .unwrap_or_else(|| "—".into())
}
fn fraction(value: f64, low: f64, high: f64) -> f64 {
    if value.is_finite() {
        (value - low) / (high - low).max(1.0)
    } else {
        0.0
    }
}
fn bounds(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let values: Vec<_> = values.filter(|v| v.is_finite()).collect();
    (
        values.iter().copied().reduce(f64::min).unwrap_or(0.0),
        values.into_iter().reduce(f64::max).unwrap_or(1.0),
    )
}
fn palette(code: u16, day: bool) -> &'static str {
    if !day {
        "night"
    } else if code >= 95 {
        "storm"
    } else if (51..=67).contains(&code) || (80..=86).contains(&code) {
        "wet"
    } else if matches!(code, 3 | 45 | 48) {
        "overcast"
    } else {
        "clear"
    }
}
pub(super) fn weather(mut weather: WeatherState, now: i64) -> WeatherState {
    weather = super::astronomy::weather(weather, now);
    if !weather.available {
        weather.display = json!({});
        return weather;
    }
    let future: Vec<_> = weather
        .hourly
        .iter()
        .enumerate()
        .filter(|(_, h)| h.time_unix_ms >= now.saturating_sub(900_000))
        .map(|(i, _)| i)
        .collect();
    let indices: Vec<_> = if future.is_empty() {
        (0..weather.hourly.len()).take(8).collect()
    } else {
        future.into_iter().take(8).collect()
    };
    let (low, high) = bounds(indices.iter().map(|i| weather.hourly[*i].temperature_c));
    for hour in &mut weather.hourly {
        hour.display = json!({"temperature":number(hour.temperature_c,"°"), "precipitation":number(f64::from(hour.precipitation_probability),"%"), "time":local(hour.time_unix_ms,&weather.timezone,"%H:%M"), "temperature_fraction":fraction(hour.temperature_c,low,high)});
    }
    let (low, _) = bounds(weather.daily.iter().take(7).map(|d| d.low_c));
    let (_, high) = bounds(weather.daily.iter().take(7).map(|d| d.high_c));
    for day in &mut weather.daily {
        day.display = json!({"low":number(day.low_c,"°"),"high":number(day.high_c,"°"),"precipitation":number(f64::from(day.precipitation_probability),"%"),"day":local(day.date_unix_ms,&weather.timezone,"%a").to_uppercase(),"low_fraction":fraction(day.low_c,low,high),"span_fraction":fraction(day.high_c-day.low_c,0.0,high-low)});
    }
    let sun_valid = weather.sunrise_unix_ms > 0 && weather.sunset_unix_ms > weather.sunrise_unix_ms;
    let daylight = if sun_valid {
        weather
            .sunset_unix_ms
            .saturating_sub(weather.sunrise_unix_ms)
    } else {
        0
    };
    let minutes = (daylight as f64 / 60_000.0).round() as u64;
    let compass = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"]
        [((f64::from(weather.wind_direction_degrees % 360) / 45.0).round() as usize) % 8];
    weather.display = json!({
        "temperature":number(weather.temperature_c,"°"),"apparent_temperature":number(weather.apparent_temperature_c,"°"),
        "low":number(weather.low_c,"°"),"high":number(weather.high_c,"°"),"precipitation":number(f64::from(weather.precipitation_probability),"%"),
        "wind":format!("{compass} {}",number(weather.wind_speed_kmh," km/h")),
        "time":local(now,&weather.timezone,"%H:%M"),
        "updated":local(weather.updated_unix_ms,&weather.timezone,"%H:%M"),
        "sunrise":if sun_valid {local(weather.sunrise_unix_ms,&weather.timezone,"%H:%M")} else {"—".into()},
        "sunset":if sun_valid {local(weather.sunset_unix_ms,&weather.timezone,"%H:%M")} else {"—".into()},
        "solar_noon":weather.solar_noon.as_ref().map(|s| local(s.unix_ms,&weather.timezone,"%H:%M")).unwrap_or_else(||"—".into()),
        "daylight":if sun_valid {format!("{} h {:02} min",minutes/60,minutes%60)} else {"—".into()},
        "daylight_fraction":(daylight as f64 / 86_400_000.0).min(1.0),
        "sun_progress":if sun_valid {(now.saturating_sub(weather.sunrise_unix_ms) as f64 / daylight as f64).clamp(0.0,1.0)} else {0.0},
        "sun_available":sun_valid,"hour_indices":indices,"palette":palette(weather.condition_code,weather.is_day)
    });
    weather
}

pub(super) fn clock_display(now: DateTime<chrono_tz::Tz>, offset: i32) -> serde_json::Value {
    json!({"time":now.format("%H:%M").to_string(),"date":now.format("%A, %-d %B %Y").to_string(),"offset":utc_offset(offset)})
}
fn refresh_clock(clock: &mut WorldClockState, now: DateTime<Utc>) -> bool {
    if let Ok(mut next) = WorldClockState::at(&clock.timezone, &clock.label, now) {
        // A named place is not necessarily the IANA zone's representative city.
        next.city.clone_from(&clock.city);
        *clock = next;
        true
    } else {
        clock.clock_display = json!({});
        false
    }
}
pub(crate) fn update(state: &mut ActivityState, now: DateTime<Utc>) {
    for clock in &mut state.world_clocks {
        refresh_clock(clock, now);
    }
    state.weather = weather(state.weather.clone(), now.timestamp_millis());
    for value in &mut state.weather_locations {
        *value = weather(value.clone(), now.timestamp_millis());
    }
    for location in &mut state.locations {
        location.clock_unavailable = !refresh_clock(&mut location.clock, now);
        if let Some(value) = &mut location.weather {
            *value = weather(value.clone(), now.timestamp_millis());
        }
        location.subtitle = subtitle(location);
    }
}
fn subtitle(location: &super::locations::Location) -> String {
    let zone = if location.clock_unavailable || location.clock.timezone.is_empty() {
        "Timezone unavailable"
    } else {
        &location.clock.timezone
    };
    if !location.current || location.home {
        return zone.into();
    }
    use crate::model::LocationSource;
    let automatic = location.location_mode.as_deref() == Some("automatic");
    let mut label = match location.location_source {
        Some(LocationSource::Manual) => {
            if automatic {
                "last manual location"
            } else {
                "Manual location"
            }
        }
        Some(LocationSource::Ip) => "IP estimate",
        Some(LocationSource::NonIp) => "Location estimate (non-IP)",
        _ => "Location estimate (source unknown)",
    }
    .to_string();
    if location.location_source != Some(LocationSource::Manual)
        && let Some(meters) = location
            .accuracy_meters
            .filter(|v| v.is_finite() && *v >= 0.0)
    {
        let distance = if meters < 1000.0 {
            number(meters, " m")
        } else if meters < 10000.0 {
            format!(
                "{} km",
                crate::presentation::fixed_decimal(meters / 1000.0, 1)
                    .unwrap()
                    .trim_end_matches(".0")
            )
        } else {
            number(meters / 1000.0, " km")
        };
        label.push_str(&format!(" · reported accuracy ~{distance}"));
    }
    format!(
        "{}{label} · {zone}",
        if automatic { "Automatic · " } else { "" }
    )
}
pub(super) fn next_minute(now: DateTime<Utc>) -> std::time::Duration {
    std::time::Duration::from_millis(
        60_000 - u64::from(now.second()) * 1000 - u64::from(now.timestamp_subsec_millis()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    #[test]
    fn hourly_window_and_chart_ranges_share_one_observation_instant() {
        use super::super::model::{WeatherDay, WeatherHour};
        let now = 1_766_400_000_000;
        let input = WeatherState {
            available: true,
            timezone: "Europe/Amsterdam".into(),
            hourly: (0..13)
                .map(|i| WeatherHour {
                    time_unix_ms: now - 1_800_000 + i * 3_600_000,
                    temperature_c: i as f64 - 4.0,
                    ..Default::default()
                })
                .collect(),
            daily: vec![
                WeatherDay {
                    low_c: -4.0,
                    high_c: 4.0,
                    ..Default::default()
                },
                WeatherDay {
                    low_c: 0.0,
                    high_c: 12.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let projected = weather(input.clone(), now);
        assert_eq!(
            projected.display["hour_indices"],
            json!([1, 2, 3, 4, 5, 6, 7, 8])
        );
        assert_eq!(projected.hourly[1].display["temperature_fraction"], 0.0);
        assert_eq!(projected.hourly[8].display["temperature_fraction"], 1.0);
        assert_eq!(projected.hourly[4].display["temperature"], "0°");
        assert_eq!(projected.daily[0].display["span_fraction"], 0.5);
        assert_eq!(projected.daily[1].display["low_fraction"], 0.25);
        assert_eq!(projected.display["daylight"], "—");
        assert_eq!(
            weather(input, now + 100_000_000).display["hour_indices"],
            json!([0, 1, 2, 3, 4, 5, 6, 7])
        );
    }
    #[test]
    fn clock_projection_preserves_named_places_and_handles_dst_and_unknown_zones() {
        use super::super::locations::Location;
        let before = Utc.with_ymd_and_hms(2026, 3, 29, 0, 59, 59).unwrap();
        let mut state = ActivityState {
            locations: vec![Location {
                id: "city".into(),
                clock: WorldClockState {
                    timezone: "Europe/Amsterdam".into(),
                    label: "Haarlem".into(),
                    city: "Haarlem".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        update(&mut state, before);
        assert_eq!(state.locations[0].clock.clock_display["time"], "01:59");
        assert_eq!(next_minute(before), std::time::Duration::from_secs(1));
        update(&mut state, before + chrono::Duration::seconds(1));
        assert_eq!(state.locations[0].clock.clock_display["time"], "03:00");
        assert_eq!(state.locations[0].clock.clock_display["offset"], "UTC+2");
        assert_eq!(state.locations[0].clock.city, "Haarlem");
        state.locations[0].clock.timezone = "invalid".into();
        update(&mut state, before);
        assert!(state.locations[0].clock_unavailable);
        assert_eq!(state.locations[0].subtitle, "Timezone unavailable");
        assert_eq!(state.locations[0].clock.clock_display, json!({}));
    }
    #[test]
    fn accuracy_units_do_not_turn_missing_or_invalid_readings_into_zero() {
        use super::super::locations::Location;
        let mut city = Location {
            current: true,
            clock: WorldClockState {
                timezone: "Etc/UTC".into(),
                ..Default::default()
            },
            location_mode: Some("automatic".into()),
            location_source: Some(crate::model::LocationSource::Ip),
            ..Default::default()
        };
        assert_eq!(subtitle(&city), "Automatic · IP estimate · Etc/UTC");
        city.accuracy_meters = Some(1250.0);
        assert_eq!(
            subtitle(&city),
            "Automatic · IP estimate · reported accuracy ~1.3 km · Etc/UTC"
        );
        city.accuracy_meters = Some(0.0);
        assert!(subtitle(&city).contains("~0 m"));
        city.accuracy_meters = Some(f64::NAN);
        assert!(!subtitle(&city).contains("accuracy"));
    }
    #[test]
    fn native_projection_handles_rounding_daylight_and_selection() {
        let now = Utc.with_ymd_and_hms(2026, 3, 29, 1, 30, 0).unwrap();
        let value = WeatherState {
            available: true,
            timezone: "Europe/Amsterdam".into(),
            temperature_c: -1.5,
            wind_direction_degrees: 360,
            wind_speed_kmh: 12.5,
            sunrise_unix_ms: now.timestamp_millis(),
            sunset_unix_ms: now.timestamp_millis() + 43_200_000,
            ..Default::default()
        };
        let projected = weather(value, now.timestamp_millis());
        assert_eq!(projected.display["temperature"], "-1°");
        assert_eq!(projected.display["wind"], "N 13 km/h");
        assert_eq!(projected.display["daylight"], "12 h 00 min");
        assert_eq!(projected.display["daylight_fraction"], 0.5);
        assert_eq!(projected.display["sunrise"], "03:30");
        assert_eq!(next_minute(now), std::time::Duration::from_secs(60));
        assert_eq!(local(now.timestamp_millis(), "invalid", "%H:%M"), "—");
    }
}
