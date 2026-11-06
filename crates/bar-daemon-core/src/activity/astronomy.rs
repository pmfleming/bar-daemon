//! Approximate, timestamped metadata; no ephemeris accuracy is implied.
use chrono::{DateTime, Datelike, Offset, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

const DAY_MS: i64 = 86_400_000;
const SYNODIC_DAYS: f64 = 29.530_588_853;
const NEW_MOON_MS: i64 = 947_182_440_000; // 2000-01-06 18:14 UTC

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    let phase = [
        LunarPhaseName::NewMoon,
        LunarPhaseName::WaxingCrescent,
        LunarPhaseName::FirstQuarter,
        LunarPhaseName::WaxingGibbous,
        LunarPhaseName::FullMoon,
        LunarPhaseName::WaningGibbous,
        LunarPhaseName::LastQuarter,
        LunarPhaseName::WaningCrescent,
    ][(fraction * 8.0).round() as usize % 8];
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
