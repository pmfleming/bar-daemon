use super::ForecastResponse;
use crate::activity::config::WeatherConfig;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

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

#[tokio::test]
async fn cache_hits_and_failures_reuse_forecast_buffers_without_polling_fresh_fetches() {
    let config = WeatherConfig {
        id: "home".into(),
        location: "Amsterdam".into(),
        ..Default::default()
    };
    let previous = response().normalize(&config, now()).unwrap();
    let buffers = (previous.hourly.as_ptr(), previous.daily.as_ptr());
    let updated = previous.updated_unix_ms;
    let cached = super::cached_or_fetch(&config, Some(previous), updated + 899_999, async {
        panic!("fresh cache must not poll HTTP")
    })
    .await;
    assert_eq!((cached.hourly.as_ptr(), cached.daily.as_ptr()), buffers);
    let stale = super::cached_or_fetch(
        &config,
        Some(cached),
        updated + 900_000,
        std::future::ready(Err(anyhow::anyhow!("offline"))),
    )
    .await;
    assert_eq!((stale.hourly.as_ptr(), stale.daily.as_ptr()), buffers);
    assert_eq!(stale.updated_unix_ms, updated);
    assert_eq!(stale.error.as_deref(), Some("offline"));
    let mut missing = super::cached_or_fetch(
        &config,
        None,
        updated,
        std::future::ready(Err(anyhow::anyhow!("offline"))),
    )
    .await;
    assert!(!missing.available);
    assert_eq!(missing.id, config.id);
    // Unavailable entries must fetch even when their timestamp is fresh.
    missing.updated_unix_ms = updated;
    let recovered = super::cached_or_fetch(
        &config,
        Some(missing),
        updated,
        std::future::ready(Ok(stale)),
    )
    .await;
    assert!(recovered.available);
    assert_eq!(
        (recovered.hourly.as_ptr(), recovered.daily.as_ptr()),
        buffers
    );
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
