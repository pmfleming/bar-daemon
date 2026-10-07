use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
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

fn forecast_request(client: &reqwest::Client, config: &WeatherConfig) -> reqwest::RequestBuilder {
    client.get(FORECAST_URL)
        .query(&[("latitude", config.latitude), ("longitude", config.longitude)])
        .query(&[
            ("timezone", config.timezone.as_str()),
            ("timeformat", "unixtime"),
            ("forecast_days", "7"),
            ("current", "temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,is_day,wind_speed_10m,wind_direction_10m,wind_gusts_10m"),
            ("hourly", "temperature_2m,precipitation_probability,precipitation,wind_speed_10m,wind_direction_10m,weather_code,is_day"),
            ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset"),
        ])
        .timeout(std::time::Duration::from_secs(12))
}

pub(crate) async fn fetch(config: &WeatherConfig) -> Result<WeatherState> {
    forecast_request(&reqwest::Client::new(), config)
        .send()
        .await
        .context("request Open-Meteo forecast")?
        .error_for_status()
        .context("Open-Meteo forecast status")?
        .json::<ForecastResponse>()
        .await
        .context("decode Open-Meteo forecast")?
        .normalize(config, Utc::now())
}

impl ForecastResponse {
    // All time-dependent normalization uses the supplied instant, not another
    // clock read. This boundary is testable without HTTP or the daemon state.
    fn normalize(self, config: &WeatherConfig, now: DateTime<Utc>) -> Result<WeatherState> {
        let now_ms = now.timestamp_millis();
        let hourly = self.hourly.hours(now_ms)?;
        let daily = self.daily.days()?;
        let default_day = WeatherDay::default();
        let today = daily.first().unwrap_or(&default_day);
        Ok(WeatherState {
            available: true,
            id: config.id.clone(),
            location: config.location.clone(),
            home: config.home,
            timezone: self.timezone,
            utc_offset_seconds: self.utc_offset_seconds,
            timezone_region_ids: crate::timezone_regions::ids_for_offset(
                self.utc_offset_seconds,
                now,
            ),
            latitude: config.latitude,
            longitude: config.longitude,
            condition: condition(self.current.weather_code).into(),
            condition_code: self.current.weather_code,
            is_day: self.current.is_day != 0,
            temperature_c: self.current.temperature_2m,
            apparent_temperature_c: self.current.apparent_temperature,
            high_c: today.high_c,
            low_c: today.low_c,
            precipitation_probability: today.precipitation_probability,
            precipitation_mm: self.current.precipitation,
            wind_speed_kmh: self.current.wind_speed_10m,
            wind_direction_degrees: self.current.wind_direction_10m,
            wind_gust_kmh: self.current.wind_gusts_10m,
            humidity_percent: self.current.relative_humidity_2m,
            sunrise_unix_ms: today.sunrise_unix_ms,
            sunset_unix_ms: today.sunset_unix_ms,
            updated_unix_ms: now_ms,
            solar_noon: None, // Refreshed against the local date when activity is published.
            hourly,
            daily,
            error: None,
        })
    }
}

impl HourlyWeather {
    fn hours(&self, now_ms: i64) -> Result<Vec<WeatherHour>> {
        let earliest = now_ms.saturating_sub(30 * 60 * 1_000);
        let mut hours = Vec::new();
        for (index, time) in self.time.iter().enumerate() {
            let time_unix_ms = milliseconds(*time)?;
            if time_unix_ms < earliest {
                continue;
            }
            hours.push(self.hour(index, time_unix_ms));
            if hours.len() == 12 {
                break;
            }
        }
        Ok(hours)
    }

    fn hour(&self, index: usize, time_unix_ms: i64) -> WeatherHour {
        let condition_code = value(&self.weather_code, index);
        WeatherHour {
            time_unix_ms,
            temperature_c: value(&self.temperature_2m, index),
            precipitation_probability: value(&self.precipitation_probability, index),
            precipitation_mm: value(&self.precipitation, index),
            wind_speed_kmh: value(&self.wind_speed_10m, index),
            wind_direction_degrees: value(&self.wind_direction_10m, index),
            condition: condition(condition_code).into(),
            condition_code,
            is_day: value(&self.is_day, index) != 0,
        }
    }
}

impl DailyWeather {
    fn days(&self) -> Result<Vec<WeatherDay>> {
        self.time
            .iter()
            .enumerate()
            .take(7)
            .map(|(index, time)| {
                let condition_code = value(&self.weather_code, index);
                Ok(WeatherDay {
                    date_unix_ms: milliseconds(*time)?,
                    high_c: value(&self.temperature_2m_max, index),
                    low_c: value(&self.temperature_2m_min, index),
                    precipitation_probability: value(&self.precipitation_probability_max, index),
                    condition: condition(condition_code).into(),
                    condition_code,
                    sunrise_unix_ms: milliseconds(value(&self.sunrise, index))?,
                    sunset_unix_ms: milliseconds(value(&self.sunset, index))?,
                })
            })
            .collect()
    }
}

fn milliseconds(seconds: i64) -> Result<i64> {
    seconds
        .checked_mul(1_000)
        .context("Open-Meteo timestamp outside millisecond range")
}

// Preserve the existing default for short provider columns and empty days.
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
mod tests;
