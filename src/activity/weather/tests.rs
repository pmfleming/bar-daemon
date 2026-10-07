use super::{ForecastResponse, condition, forecast_request};
use crate::activity::config::WeatherConfig;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};

fn payload() -> Value {
    json!({
        "timezone": "Europe/Amsterdam", "utc_offset_seconds": 3600,
        "current": {
            "temperature_2m": 8.5, "apparent_temperature": 7.0,
            "relative_humidity_2m": 75, "precipitation": 0.2,
            "weather_code": 3, "is_day": 1, "wind_speed_10m": 12.0,
            "wind_direction_10m": 270, "wind_gusts_10m": 20.0
        },
        "hourly": {
            "time": [-1, 0, 3600, 7200], "temperature_2m": [99.0, 10.0],
            "precipitation_probability": [0, 60], "precipitation": [0.0, 0.5],
            "wind_speed_10m": [0.0, 15.0], "wind_direction_10m": [0, 180],
            "weather_code": [0, 45], "is_day": [0, 1]
        },
        "daily": {
            "time": [0, 86400], "weather_code": [61],
            "temperature_2m_max": [17.0], "temperature_2m_min": [3.0],
            "precipitation_probability_max": [80], "sunrise": [21600], "sunset": [64800]
        }
    })
}

fn response() -> ForecastResponse {
    serde_json::from_value(payload()).unwrap()
}

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_800_000).unwrap()
}

#[test]
fn normalization_preserves_units_indices_and_short_column_defaults() {
    let config = WeatherConfig {
        id: "home".into(),
        location: "Amsterdam".into(),
        home: true,
        latitude: 52.0,
        longitude: 4.0,
        ..Default::default()
    };
    let weather = response().normalize(&config, now()).unwrap();
    assert!(weather.available);
    assert_eq!(
        (&weather.id, &weather.location, weather.home),
        (&config.id, &config.location, true)
    );
    assert_eq!((weather.latitude, weather.longitude), (52.0, 4.0));
    assert_eq!(weather.timezone, "Europe/Amsterdam");
    assert_eq!(weather.utc_offset_seconds, 3600);
    assert_eq!(weather.updated_unix_ms, 1_800_000);
    assert_eq!(
        (
            weather.condition.as_str(),
            weather.condition_code,
            weather.is_day
        ),
        ("Overcast", 3, true)
    );
    assert_eq!(
        (
            weather.temperature_c,
            weather.apparent_temperature_c,
            weather.humidity_percent
        ),
        (8.5, 7.0, 75)
    );
    assert_eq!(
        (
            weather.high_c,
            weather.low_c,
            weather.precipitation_probability
        ),
        (17.0, 3.0, 80)
    );
    assert_eq!(
        (
            weather.precipitation_mm,
            weather.wind_speed_kmh,
            weather.wind_direction_degrees,
            weather.wind_gust_kmh
        ),
        (0.2, 12.0, 270, 20.0)
    );
    assert_eq!(
        (weather.sunrise_unix_ms, weather.sunset_unix_ms),
        (21_600_000, 64_800_000)
    );
    assert!(weather.solar_noon.is_none());
    assert!(weather.error.is_none());
    assert_eq!(weather.hourly.len(), 3);
    let hour = &weather.hourly[0];
    assert_eq!((hour.time_unix_ms, hour.temperature_c), (0, 10.0));
    assert_eq!(
        (hour.condition.as_str(), hour.condition_code, hour.is_day),
        ("Fog", 45, true)
    );
    assert_eq!(
        (
            hour.precipitation_probability,
            hour.precipitation_mm,
            hour.wind_speed_kmh,
            hour.wind_direction_degrees
        ),
        (60, 0.5, 15.0, 180)
    );
    assert_eq!(weather.hourly[1].temperature_c, 0.0);
    assert_eq!(weather.daily[0].condition, "Rain");
    assert_eq!(weather.daily[1].date_unix_ms, 86_400_000);
    assert_eq!(weather.daily[1].sunrise_unix_ms, 0);
    assert_eq!(
        weather.timezone_region_ids,
        crate::timezone_regions::ids_for_offset(3600, now())
    );
}

#[test]
fn horizon_and_half_hour_boundary_are_exact_and_empty_days_remain_valid() {
    let mut forecast = response();
    forecast.hourly.time = (0..20).map(|hour| hour * 3600).collect();
    forecast.daily.time = (0..9).map(|day| day * 86400).collect();
    let weather = forecast
        .normalize(&WeatherConfig::default(), now())
        .unwrap();
    assert_eq!(weather.hourly.len(), 12);
    assert_eq!(weather.hourly[11].time_unix_ms, 39_600_000);
    assert_eq!(weather.daily.len(), 7);
    let later = now() + chrono::Duration::milliseconds(1);
    let weather = response()
        .normalize(&WeatherConfig::default(), later)
        .unwrap();
    assert_eq!(weather.hourly[0].time_unix_ms, 3_600_000);
    let mut empty = response();
    empty.hourly.time.clear();
    empty.daily.time.clear();
    let weather = empty.normalize(&WeatherConfig::default(), now()).unwrap();
    assert!(weather.available && weather.hourly.is_empty() && weather.daily.is_empty());
    assert_eq!(
        (weather.high_c, weather.low_c, weather.sunrise_unix_ms),
        (0.0, 0.0, 0)
    );
}

#[test]
fn malformed_payloads_and_overflowing_provider_times_are_errors() {
    assert!(serde_json::from_slice::<ForecastResponse>(b"not JSON").is_err());
    for field in [
        "current",
        "hourly",
        "daily",
        "timezone",
        "utc_offset_seconds",
    ] {
        let mut invalid = payload();
        invalid.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ForecastResponse>(invalid).is_err(),
            "{field}"
        );
    }
    for value in [i64::MIN, i64::MAX] {
        for field in ["hour", "day", "sunrise", "sunset"] {
            let mut forecast = response();
            match field {
                "hour" => forecast.hourly.time[0] = value,
                "day" => forecast.daily.time[0] = value,
                "sunrise" => forecast.daily.sunrise[0] = value,
                _ => forecast.daily.sunset[0] = value,
            }
            assert!(
                forecast
                    .normalize(&WeatherConfig::default(), now())
                    .is_err(),
                "{field}"
            );
        }
    }
}

#[test]
fn request_uses_fixed_endpoint_units_and_timeout_without_io() {
    let config = WeatherConfig {
        latitude: -33.5,
        longitude: 151.25,
        timezone: "Europe/Amsterdam".into(),
        ..Default::default()
    };
    let request = forecast_request(&reqwest::Client::new(), &config)
        .build()
        .unwrap();
    assert_eq!(request.url().host_str(), Some("api.open-meteo.com"));
    assert_eq!(request.url().scheme(), "https");
    assert_eq!(request.url().path(), "/v1/forecast");
    assert_eq!(request.timeout(), Some(&Duration::from_secs(12)));
    let query: HashMap<_, _> = request.url().query_pairs().collect();
    for (key, expected) in [
        ("latitude", "-33.5"),
        ("longitude", "151.25"),
        ("timezone", "Europe/Amsterdam"),
        ("timeformat", "unixtime"),
        ("forecast_days", "7"),
    ] {
        assert_eq!(query[key], expected);
    }
    assert_eq!(query.len(), 8);
    assert!(query["current"].contains("relative_humidity_2m"));
    assert!(query["hourly"].contains("precipitation_probability"));
    assert!(query["daily"].contains("sunrise,sunset"));
}

#[test]
fn known_weather_codes_and_unknown_fallback_keep_their_labels() {
    for (code, expected) in [
        (0, "Clear"),
        (1, "Mostly clear"),
        (2, "Partly cloudy"),
        (3, "Overcast"),
        (48, "Fog"),
        (57, "Drizzle"),
        (67, "Rain"),
        (77, "Snow"),
        (82, "Rain showers"),
        (86, "Snow showers"),
        (99, "Thunderstorm"),
        (65535, "Unknown"),
    ] {
        assert_eq!(condition(code), expected);
    }
}
