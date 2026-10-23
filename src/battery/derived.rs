//! Authoritative aggregate-battery energy/forecast semantics, independent of QML.
use crate::model::{BatteryHistoryPoint, BatteryState, ChargeForecast, EnergyBin, EnergyHistory};
use std::collections::BTreeMap;

pub(crate) fn forecast(battery: &BatteryState) -> ChargeForecast {
    let protection = &battery.protection;
    let limit = protection.end_percent.filter(|value| {
        protection.enabled && !protection.charge_once_active && (1..100).contains(value)
    });
    let discharging = !battery.plugged && !battery.charging;
    let target = if discharging { 0 } else { limit.unwrap_or(100) };
    let estimate = forecast_seconds(battery, target);
    let at_limit = battery.available && limit == Some(target) && battery.percentage >= target;
    let status = match estimate {
        Some(seconds) if seconds > 0.0 => "valid",
        Some(_) => "estimating",
        None if at_limit => "limit-reached",
        None if battery.available && battery.plugged && battery.percentage == 100 => "full",
        None => "unavailable",
    };
    ChargeForecast {
        duration_label: crate::presentation::estimate(estimate.unwrap_or(0.0)),
        limit,
        target,
        percentage: battery.percentage,
        seconds: estimate.unwrap_or(0.0),
        estimating: status == "estimating",
        status: status.into(),
        approximate: status == "valid",
        scope: "aggregate".into(),
    }
}

// None means no active forecast; zero means an active forecast awaiting telemetry.
fn forecast_seconds(battery: &BatteryState, target: u8) -> Option<f64> {
    if !battery.available {
        return None;
    }
    let (remaining, maximum, distance, full_distance) = if target == 0 {
        if battery.percentage == 0 {
            return None;
        }
        (battery.time_to_empty_seconds, 604800, 1, 1)
    } else {
        if !battery.charging || battery.percentage >= target {
            return None;
        }
        (
            battery.time_to_full_seconds,
            86400,
            target - battery.percentage,
            100 - battery.percentage,
        )
    };
    Some(if (1..=maximum).contains(&remaining) {
        remaining as f64 * f64::from(distance) / f64::from(full_distance)
    } else {
        0.0
    })
}

fn power_available(point: &BatteryHistoryPoint) -> bool {
    // Legacy positive samples are measurements. Legacy zero was also used for
    // invalid input, so only a newly recorded validity flag can certify zero.
    point.power_valid.unwrap_or(point.power_watts > 0.0)
        && point.power_watts.is_finite()
        && point.power_watts >= 0.0
}
fn discharge(point: &BatteryHistoryPoint) -> bool {
    point.mode == "discharging" || (point.mode.is_empty() && !point.charging && !point.plugged)
}

// Only continuous, ordered, measured discharge samples can contribute energy.
struct DischargeSegment {
    start: u64,
    end: u64,
    start_watts: f64,
    end_watts: f64,
}

impl DischargeSegment {
    fn between(
        previous: &BatteryHistoryPoint,
        point: &BatteryHistoryPoint,
        origin: u64,
    ) -> Option<Self> {
        if previous.timestamp_ms == 0
            || point.timestamp_ms <= previous.timestamp_ms
            || !point.continuous
            || point.active_time_ms <= previous.active_time_ms
            || !discharge(previous)
            || !discharge(point)
            || !power_available(previous)
            || !power_available(point)
        {
            return None;
        }
        Some(Self {
            start: previous.active_time_ms.saturating_sub(origin),
            end: point.active_time_ms.saturating_sub(origin),
            start_watts: previous.power_watts,
            end_watts: point.power_watts,
        })
    }

    fn power_at(&self, time: u64) -> f64 {
        self.start_watts
            + (self.end_watts - self.start_watts) * (time - self.start) as f64
                / (self.end - self.start) as f64
    }

    fn accumulate(&self, bins: &mut BTreeMap<u64, EnergyBin>, duration: u64, interval: u64) {
        let mut offset = self.start;
        while offset < self.end {
            let index = offset / interval;
            let boundary = (index + 1).saturating_mul(interval);
            let stop = self.end.min(boundary);
            let bin = bins.entry(index).or_insert_with(|| EnergyBin {
                x0: (index * interval) as f64 / duration as f64,
                x1: duration.min(boundary) as f64 / duration as f64,
                ..Default::default()
            });
            bin.value += (self.power_at(offset) / 2.0 + self.power_at(stop) / 2.0)
                * (stop - offset) as f64
                / 3_600_000.0;
            bin.observed_ms = bin.observed_ms.saturating_add(stop - offset);
            offset = stop;
        }
    }
}

pub(crate) fn energy(points: &[BatteryHistoryPoint]) -> EnergyHistory {
    let (first, last) = points
        .iter()
        .filter(|p| p.timestamp_ms > 0)
        .map(|p| (p.active_time_ms, p.active_time_ms))
        .reduce(|(first, last), (time, _)| (first.min(time), last.max(time)))
        .unwrap_or_default();
    let duration = last - first;
    let interval = duration.div_ceil(48 * 900_000).max(1) * 900_000;
    let mut bins: BTreeMap<u64, EnergyBin> = BTreeMap::new();
    for (previous, point) in points.iter().zip(points.iter().skip(1)) {
        if let Some(segment) = DischargeSegment::between(previous, point, first) {
            segment.accumulate(&mut bins, duration, interval);
        }
    }
    let bars: Vec<_> = bins
        .into_values()
        .filter(|bin| bin.value.is_finite())
        .collect();
    EnergyHistory {
        maximum: bars.iter().map(|b| b.value).fold(0.0, f64::max),
        total_wh: bars.iter().map(|b| b.value).sum(),
        observed_ms: bars.iter().map(|b| b.observed_ms).sum(),
        bars,
        interval_ms: interval,
        active_duration_ms: duration,
    }
}
