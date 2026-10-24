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
    values
        .filter(|v| v.is_finite())
        .fold(None, |range, value| {
            let (low, high) = range.unwrap_or((value, value));
            Some((value.min(low), value.max(high)))
        })
        .unwrap_or((0.0, 1.0))
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
fn forecast(weather: &mut WeatherState, now: i64) -> Vec<usize> {
    let mut indices: Vec<_> = weather
        .hourly
        .iter()
        .enumerate()
        .filter(|(_, h)| h.time_unix_ms >= now.saturating_sub(900_000))
        .map(|(i, _)| i)
        .take(8)
        .collect();
    if indices.is_empty() {
        indices.extend(0..weather.hourly.len().min(8));
    }
    let (low, high) = bounds(indices.iter().map(|i| weather.hourly[*i].temperature_c));
    for hour in &mut weather.hourly {
        hour.display = json!({"temperature":number(hour.temperature_c,"°"), "precipitation":number(f64::from(hour.precipitation_probability),"%"), "time":local(hour.time_unix_ms,&weather.timezone,"%H:%M"), "temperature_fraction":fraction(hour.temperature_c,low,high)});
    }
    let (low, _) = bounds(weather.daily.iter().take(7).map(|d| d.low_c));
    let (_, high) = bounds(weather.daily.iter().take(7).map(|d| d.high_c));
    for day in &mut weather.daily {
        day.display = json!({"low":number(day.low_c,"°"),"high":number(day.high_c,"°"),"precipitation":number(f64::from(day.precipitation_probability),"%"),"day":local(day.date_unix_ms,&weather.timezone,"%a").to_uppercase(),"low_fraction":fraction(day.low_c,low,high),"span_fraction":fraction(day.high_c-day.low_c,0.0,high-low)});
    }
    indices
}
fn weather(weather: &mut WeatherState, now: i64) {
    weather.solar_noon = weather
        .available
        .then(|| {
            super::astronomy::solar_noon(
                weather.sunrise_unix_ms,
                weather.sunset_unix_ms,
                now,
                &weather.timezone,
            )
        })
        .flatten();
    if !weather.available {
        weather.display = json!({});
        return;
    }
    let indices = forecast(weather, now);
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
    for value in std::iter::once(&mut state.weather).chain(&mut state.weather_locations) {
        weather(value, now.timestamp_millis());
    }
    for location in &mut state.locations {
        location.clock_unavailable = !refresh_clock(&mut location.clock, now);
        if let Some(value) = &mut location.weather {
            weather(value, now.timestamp_millis());
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
    let mut label = match (location.location_source, automatic) {
        (Some(LocationSource::Manual), true) => "last manual location",
        (Some(LocationSource::Manual), false) => "Manual location",
        (Some(LocationSource::Ip), _) => "IP estimate",
        (Some(LocationSource::NonIp), _) => "Location estimate (non-IP)",
        _ => "Location estimate (source unknown)",
    }
    .to_string();
    if location.location_source != Some(LocationSource::Manual)
        && let Some(distance) = location.accuracy_meters.and_then(accuracy)
    {
        label.push_str(&format!(" · reported accuracy ~{distance}"));
    }
    format!(
        "{}{label} · {zone}",
        if automatic { "Automatic · " } else { "" }
    )
}
fn accuracy(meters: f64) -> Option<String> {
    if !meters.is_finite() || meters < 0.0 {
        return None;
    }
    Some(if meters < 1000.0 {
        number(meters, " m")
    } else if meters < 10000.0 {
        format!(
            "{} km",
            crate::presentation::measurement(meters / 1000.0, "")?.trim_end_matches(".0")
        )
    } else {
        number(meters / 1000.0, " km")
    })
}
pub(super) fn next_minute(now: DateTime<Utc>) -> std::time::Duration {
    std::time::Duration::from_millis(
        60_000 - u64::from(now.second()) * 1000 - u64::from(now.timestamp_subsec_millis()),
    )
}

#[cfg(test)]
mod tests {
    use super::{local, next_minute, palette, subtitle, update, weather};
    use crate::activity::{
        locations::Location,
        model::{ActivityState, WeatherDay, WeatherHour, WeatherState, WorldClockState},
    };
    use crate::model::LocationSource;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    #[test]
    fn hourly_window_and_chart_ranges_share_one_observation_instant() {
        let instant = Utc.with_ymd_and_hms(2026, 3, 29, 1, 30, 0).unwrap();
        let now = instant.timestamp_millis();
        let mut projected = WeatherState {
            available: true,
            timezone: "Europe/Amsterdam".into(),
            temperature_c: -1.5,
            wind_direction_degrees: 360,
            wind_speed_kmh: 12.5,
            sunrise_unix_ms: now,
            sunset_unix_ms: now + 43_200_000,
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
        let allocation = projected.hourly.as_ptr();
        weather(&mut projected, now);
        assert_eq!(projected.hourly.as_ptr(), allocation);
        assert_eq!(projected.display["temperature"], "-1°");
        assert_eq!(projected.display["wind"], "N 13 km/h");
        assert_eq!(projected.display["daylight"], "12 h 00 min");
        assert_eq!(projected.display["daylight_fraction"], 0.5);
        assert_eq!(projected.display["sunrise"], "03:30");
        assert_eq!(next_minute(instant), std::time::Duration::from_secs(60));
        assert_eq!(local(now, "invalid", "%H:%M"), "—");
        assert_eq!(
            projected.display["hour_indices"],
            json!([1, 2, 3, 4, 5, 6, 7, 8])
        );
        assert_eq!(projected.hourly[1].display["temperature_fraction"], 0.0);
        assert_eq!(projected.hourly[8].display["temperature_fraction"], 1.0);
        assert_eq!(projected.hourly[4].display["temperature"], "0°");
        assert_eq!(projected.daily[0].display["span_fraction"], 0.5);
        assert_eq!(projected.daily[1].display["low_fraction"], 0.25);
        projected.sunset_unix_ms = 0;
        projected.hourly[0].temperature_c = f64::NAN;
        weather(&mut projected, now + 100_000_000);
        assert_eq!(projected.display["daylight"], "—");
        assert_eq!(projected.hourly[0].display["temperature_fraction"], 0.0);
        assert_eq!(
            projected.display["hour_indices"],
            json!([0, 1, 2, 3, 4, 5, 6, 7])
        );
        for (code, day, expected) in [
            (0, false, "night"),
            (95, true, "storm"),
            (51, true, "wet"),
            (80, true, "wet"),
            (45, true, "overcast"),
            (0, true, "clear"),
        ] {
            assert_eq!(palette(code, day), expected);
        }
        projected.available = false;
        weather(&mut projected, now);
        assert_eq!(projected.display, json!({}));
        assert!(projected.solar_noon.is_none());
    }
    #[test]
    fn clock_projection_preserves_named_places_and_handles_dst_and_unknown_zones() {
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
        for (meters, label) in [
            (0.0, "0 m"),
            (1250.0, "1.3 km"),
            (1000.0, "1 km"),
            (10000.0, "10 km"),
        ] {
            city.accuracy_meters = Some(meters);
            assert_eq!(
                subtitle(&city),
                format!("Automatic · IP estimate · reported accuracy ~{label} · Etc/UTC")
            );
        }
        for meters in [f64::NAN, -1.0, f64::INFINITY] {
            city.accuracy_meters = Some(meters);
            assert_eq!(subtitle(&city), "Automatic · IP estimate · Etc/UTC");
        }
        for (source, label) in [
            (Some(LocationSource::Manual), "last manual location"),
            (Some(LocationSource::NonIp), "Location estimate (non-IP)"),
            (None, "Location estimate (source unknown)"),
        ] {
            city.location_source = source;
            assert_eq!(subtitle(&city), format!("Automatic · {label} · Etc/UTC"));
        }
        city.location_source = Some(LocationSource::Manual);
        city.location_mode = None;
        city.accuracy_meters = Some(5.0);
        assert_eq!(subtitle(&city), "Manual location · Etc/UTC");
        city.home = true;
        assert_eq!(subtitle(&city), "Etc/UTC");
    }
}
