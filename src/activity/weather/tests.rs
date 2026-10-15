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

#[test]
fn normalization_preserves_units_and_defaults_but_rejects_time_overflow() {
    let config = WeatherConfig {
        id: "home".into(),
        location: "Amsterdam".into(),
        home: true,
        latitude: 52.0,
        longitude: 4.0,
        ..Default::default()
    };
    let mut overflowing = response();
    overflowing.hourly.time[0] = i64::MAX;
    assert!(overflowing.normalize(&config, now()).is_err());
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
