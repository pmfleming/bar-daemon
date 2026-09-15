//! Approximate, timestamped metadata; no ephemeris accuracy is implied.
use chrono::{DateTime, Datelike, Offset, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use super::model::WeatherState;

const DAY_MS: i64 = 86_400_000;
const SYNODIC_DAYS: f64 = 29.530_588_853;
const NEW_MOON_MS: i64 = 947_182_440_000; // 2000-01-06 18:14 UTC

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LunarPhaseName {
    NewMoon,
    WaxingCrescent,
    FirstQuarter,
    WaxingGibbous,
    FullMoon,
    WaningGibbous,
    LastQuarter,
    WaningCrescent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LunarPhase {
    pub phase: LunarPhaseName,
    pub fraction: f64,
    pub illumination_percent: u8,
    pub age_days: f64,
    pub evaluated_unix_ms: i64,
    pub approximate: bool,
    pub method: String,
    pub time_basis: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SolarNoon {
    pub unix_ms: i64,
    pub evaluated_unix_ms: i64,
    pub local_date: String,
    pub timezone: String,
    pub utc_offset_seconds: i32,
    pub approximate: bool,
    pub method: String,
}

fn timestamp(value: i64) -> Option<DateTime<Utc>> {
    let time = DateTime::from_timestamp_millis(value)?;
    (1..=9999).contains(&time.year()).then_some(time)
}

pub(crate) fn lunar(now: i64) -> Option<LunarPhase> {
    timestamp(now)?;
    let days = (now - NEW_MOON_MS) as f64 / DAY_MS as f64;
    let age_days = days.rem_euclid(SYNODIC_DAYS);
    let fraction = age_days / SYNODIC_DAYS;
    use LunarPhaseName::*;
    let phase = [
        NewMoon,
        WaxingCrescent,
        FirstQuarter,
        WaxingGibbous,
        FullMoon,
        WaningGibbous,
        LastQuarter,
        WaningCrescent,
    ][(fraction * 8.0).round() as usize % 8]
        .clone();
    Some(LunarPhase {
        phase,
        fraction,
        age_days,
        illumination_percent: ((1.0 - (std::f64::consts::TAU * fraction).cos()) * 50.0).round()
            as u8,
        evaluated_unix_ms: now,
        approximate: true,
        method: "mean-synodic-month".into(),
        time_basis: "utc".into(),
    })
}

pub(crate) fn solar_noon(sunrise: i64, sunset: i64, now: i64, timezone: &str) -> Option<SolarNoon> {
    let zone: Tz = timezone.parse().ok()?;
    let date = timestamp(now)?.with_timezone(&zone).date_naive();
    let duration = sunset.checked_sub(sunrise)?;
    // Missing/polar, reversed, multi-day or stale provider times are not estimates.
    if sunrise <= 0
        || !(1..=DAY_MS).contains(&duration)
        || timestamp(sunrise)?.with_timezone(&zone).date_naive() != date
        || timestamp(sunset)?.with_timezone(&zone).date_naive() != date
    {
        return None;
    }
    let unix_ms = sunrise.checked_add(duration / 2)?;
    let noon = timestamp(unix_ms)?.with_timezone(&zone);
    Some(SolarNoon {
        unix_ms,
        evaluated_unix_ms: now,
        local_date: date.to_string(),
        timezone: timezone.into(),
        utc_offset_seconds: noon.offset().fix().local_minus_utc(),
        approximate: true,
        method: "sunrise-sunset-midpoint".into(),
    })
}

pub(crate) fn weather(mut weather: WeatherState, now: i64) -> WeatherState {
    weather.solar_noon = weather
        .available
        .then(|| {
            solar_noon(
                weather.sunrise_unix_ms,
                weather.sunset_unix_ms,
                now,
                &weather.timezone,
            )
        })
        .flatten();
    weather
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn reference_quarters_and_pre_reference_dates_are_normalized() {
        for (fraction, phase, illumination) in [
            (0.0, LunarPhaseName::NewMoon, 0),
            (0.25, LunarPhaseName::FirstQuarter, 50),
            (0.5, LunarPhaseName::FullMoon, 100),
            (0.75, LunarPhaseName::LastQuarter, 50),
        ] {
            let value =
                lunar(NEW_MOON_MS + (SYNODIC_DAYS * DAY_MS as f64 * fraction) as i64).unwrap();
            assert_eq!(value.phase, phase);
            assert_eq!(value.illumination_percent, illumination);
            assert!(value.approximate);
        }
        for now in [0, -1, NEW_MOON_MS - DAY_MS, ms("9999-12-31T00:00:00Z")] {
            let value = lunar(now).unwrap();
            assert!((0.0..1.0).contains(&value.fraction));
            assert!((0.0..SYNODIC_DAYS).contains(&value.age_days));
        }
        assert!(lunar(i64::MAX).is_none());
        assert!(lunar(i64::MIN).is_none());
    }

    #[test]
    fn midpoint_uses_absolute_time_and_iana_offset_on_dst_day() {
        let rise = ms("2026-03-29T05:00:00Z");
        let set = ms("2026-03-29T17:00:00Z");
        let noon = solar_noon(rise, set, rise, "Europe/Amsterdam").unwrap();
        assert_eq!(noon.unix_ms, ms("2026-03-29T11:00:00Z"));
        assert_eq!(noon.utc_offset_seconds, 7200);
        assert_eq!(noon.local_date, "2026-03-29");
        let winter = solar_noon(
            rise - 90 * DAY_MS,
            set - 90 * DAY_MS,
            rise - 90 * DAY_MS,
            "Europe/Amsterdam",
        )
        .unwrap();
        assert_eq!(winter.utc_offset_seconds, 3600);
    }

    #[test]
    fn polar_invalid_stale_and_missing_forecasts_are_unavailable() {
        let now = ms("2026-01-01T12:00:00Z");
        for (rise, set, zone) in [
            (0, 0, "UTC"),
            (now, now - 1, "UTC"),
            (now - DAY_MS, now, "UTC"),
            (now, now + 2 * DAY_MS, "UTC"),
            (now - 1000, now + 1000, "Invalid/Zone"),
            (i64::MIN, i64::MAX, "UTC"),
        ] {
            assert!(solar_noon(rise, set, now, zone).is_none());
        }
        // UTC Jan 1 noon is already Jan 2 in this city: use its local date.
        assert!(
            solar_noon(
                now - 6 * 3_600_000,
                now + 6 * 3_600_000,
                now,
                "Pacific/Kiritimati"
            )
            .is_none()
        );
        let forecast = WeatherState {
            available: true,
            timezone: "UTC".into(),
            sunrise_unix_ms: now - 1000,
            sunset_unix_ms: now + 1000,
            ..Default::default()
        };
        assert!(weather(forecast.clone(), now).solar_noon.is_some());
        assert!(weather(forecast.clone(), now + DAY_MS).solar_noon.is_none());
        assert!(
            weather(
                WeatherState {
                    available: false,
                    ..forecast
                },
                now
            )
            .solar_noon
            .is_none()
        );
    }
}
