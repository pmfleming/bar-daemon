use anyhow::{Context, Result};
use serde::Deserialize;

use super::{
    config::WeatherConfig,
    model::{WeatherDay, WeatherHour, WeatherState},
};

const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";

#[derive(Debug, Deserialize)]
struct ForecastResponse {
    timezone: String,
    utc_offset_seconds: i32,
    current: CurrentWeather,
    hourly: HourlyWeather,
    daily: DailyWeather,
}

#[derive(Debug, Deserialize)]
struct CurrentWeather {
    temperature_2m: f64,
    apparent_temperature: f64,
    relative_humidity_2m: u8,
    precipitation: f64,
    weather_code: u16,
    is_day: u8,
    wind_speed_10m: f64,
    wind_direction_10m: u16,
    wind_gusts_10m: f64,
}

#[derive(Debug, Deserialize)]
struct HourlyWeather {
    time: Vec<i64>,
    temperature_2m: Vec<f64>,
    precipitation_probability: Vec<u8>,
    precipitation: Vec<f64>,
    wind_speed_10m: Vec<f64>,
    wind_direction_10m: Vec<u16>,
    weather_code: Vec<u16>,
    is_day: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct DailyWeather {
    time: Vec<i64>,
    weather_code: Vec<u16>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<u8>,
    sunrise: Vec<i64>,
    sunset: Vec<i64>,
}

pub(crate) async fn fetch(config: &WeatherConfig) -> Result<WeatherState> {
    let response = reqwest::Client::new()
        .get(FORECAST_URL)
        .query(&[
            ("latitude", config.latitude.to_string()),
            ("longitude", config.longitude.to_string()),
            ("timezone", config.timezone.clone()),
            ("timeformat", "unixtime".into()),
            ("forecast_days", "7".into()),
            (
                "current",
                "temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,is_day,wind_speed_10m,wind_direction_10m,wind_gusts_10m".into(),
            ),
            (
                "hourly",
                "temperature_2m,precipitation_probability,precipitation,wind_speed_10m,wind_direction_10m,weather_code,is_day".into(),
            ),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset".into(),
            ),
        ])
        .timeout(std::time::Duration::from_secs(12))
        .send()
        .await
        .context("request Open-Meteo forecast")?
        .error_for_status()
        .context("Open-Meteo forecast status")?
        .json::<ForecastResponse>()
        .await
        .context("decode Open-Meteo forecast")?;

    let now_ms = crate::time::unix_ms_i64();
    let hourly = response
        .hourly
        .time
        .iter()
        .enumerate()
        .filter(|(_, time)| **time * 1_000 >= now_ms - 30 * 60 * 1_000)
        .take(12)
        .map(|(index, time)| {
            let condition_code = value(&response.hourly.weather_code, index);
            WeatherHour {
                time_unix_ms: *time * 1_000,
                temperature_c: value(&response.hourly.temperature_2m, index),
                precipitation_probability: value(&response.hourly.precipitation_probability, index),
                precipitation_mm: value(&response.hourly.precipitation, index),
                wind_speed_kmh: value(&response.hourly.wind_speed_10m, index),
                wind_direction_degrees: value(&response.hourly.wind_direction_10m, index),
                condition: condition(condition_code).into(),
                condition_code,
                is_day: value(&response.hourly.is_day, index) != 0,
            }
        })
        .collect();
    let daily = response
        .daily
        .time
        .iter()
        .enumerate()
        .take(7)
        .map(|(index, time)| {
            let condition_code = value(&response.daily.weather_code, index);
            WeatherDay {
                date_unix_ms: *time * 1_000,
                high_c: value(&response.daily.temperature_2m_max, index),
                low_c: value(&response.daily.temperature_2m_min, index),
                precipitation_probability: value(
                    &response.daily.precipitation_probability_max,
                    index,
                ),
                condition: condition(condition_code).into(),
                condition_code,
                sunrise_unix_ms: value(&response.daily.sunrise, index) * 1_000,
                sunset_unix_ms: value(&response.daily.sunset, index) * 1_000,
            }
        })
        .collect::<Vec<_>>();
    let today = daily.first().cloned().unwrap_or_default();

    Ok(WeatherState {
        available: true,
        id: config.id.clone(),
        location: config.location.clone(),
        home: config.home,
        timezone: response.timezone,
        utc_offset_seconds: response.utc_offset_seconds,
        condition: condition(response.current.weather_code).into(),
        condition_code: response.current.weather_code,
        is_day: response.current.is_day != 0,
        temperature_c: response.current.temperature_2m,
        apparent_temperature_c: response.current.apparent_temperature,
        high_c: today.high_c,
        low_c: today.low_c,
        precipitation_probability: today.precipitation_probability,
        precipitation_mm: response.current.precipitation,
        wind_speed_kmh: response.current.wind_speed_10m,
        wind_direction_degrees: response.current.wind_direction_10m,
        wind_gust_kmh: response.current.wind_gusts_10m,
        humidity_percent: response.current.relative_humidity_2m,
        sunrise_unix_ms: today.sunrise_unix_ms,
        sunset_unix_ms: today.sunset_unix_ms,
        updated_unix_ms: now_ms,
        hourly,
        daily,
        error: None,
    })
}

fn value<T: Copy + Default>(values: &[T], index: usize) -> T {
    values.get(index).copied().unwrap_or_default()
}

fn condition(code: u16) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 | 56 | 57 => "Drizzle",
        61 | 63 | 65 | 66 | 67 => "Rain",
        71 | 73 | 75 | 77 => "Snow",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95..=99 => "Thunderstorm",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn maps_wmo_conditions() {
        assert_eq!(super::condition(0), "Clear");
        assert_eq!(super::condition(63), "Rain");
        assert_eq!(super::condition(95), "Thunderstorm");
    }
}
