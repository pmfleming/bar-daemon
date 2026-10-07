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
        if previous.timestamp_ms == 0
            || point.timestamp_ms <= previous.timestamp_ms
            || !point.continuous
            || point.active_time_ms <= previous.active_time_ms
            || !discharge(previous)
            || !discharge(point)
            || !power_available(previous)
            || !power_available(point)
        {
            continue;
        }
        let start = previous.active_time_ms.saturating_sub(first);
        let end = point.active_time_ms.saturating_sub(first);
        let mut offset = start;
        while offset < end {
            let index = offset / interval;
            let stop = end.min((index + 1).saturating_mul(interval));
            let power = |time: u64| {
                previous.power_watts
                    + (point.power_watts - previous.power_watts) * (time - start) as f64
                        / (end - start) as f64
            };
            let bin = bins.entry(index).or_insert_with(|| EnergyBin {
                x0: (index * interval) as f64 / duration as f64,
                x1: duration.min((index + 1).saturating_mul(interval)) as f64 / duration as f64,
                ..Default::default()
            });
            bin.value +=
                (power(offset) / 2.0 + power(stop) / 2.0) * (stop - offset) as f64 / 3_600_000.0;
            bin.observed_ms = bin.observed_ms.saturating_add(stop - offset);
            offset = stop;
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

#[cfg(test)]
mod tests {
    use super::{energy, forecast};
    use crate::model::{BatteryHistoryPoint, BatteryState};
    fn point(wall: u64, active: u64, watts: f64, continuous: bool) -> BatteryHistoryPoint {
        BatteryHistoryPoint {
            timestamp_ms: 1000 + wall,
            active_time_ms: active,
            power_watts: watts,
            power_valid: Some(true),
            continuous,
            mode: "discharging".into(),
            ..Default::default()
        }
    }
    #[test]
    fn integrates_ramps_across_bins_but_not_sleep_or_mode_changes() {
        let points = [
            point(0, 0, 8.0, false),
            point(900_000, 900_000, 12.0, true),
            point(172_800_000, 900_000, 8.0, false),
            point(173_700_000, 1_800_000, 8.0, true),
        ];
        let value = energy(&points);
        assert_eq!(value.total_wh, 4.5);
        assert_eq!(
            value
                .bars
                .iter()
                .map(|b| (b.x0, b.x1, b.value))
                .collect::<Vec<_>>(),
            [(0.0, 0.5, 2.5), (0.5, 1.0, 2.0)]
        );
        let ramp = energy(&[
            point(0, 0, 8.0, false),
            point(1_800_000, 1_800_000, 16.0, true),
        ]);
        assert_eq!(
            ramp.bars.iter().map(|b| b.value).collect::<Vec<_>>(),
            [2.5, 3.5]
        );
        for mode in ["charging", "holding"] {
            let mut last = point(60_000, 60_000, 8.0, true);
            last.mode = mode.into();
            assert!(energy(&[points[0].clone(), last]).bars.is_empty());
        }
        let first = point(0, 0, 8.0, false);
        let partial = energy(&[first.clone(), point(300_000, 300_000, 8.0, true)]);
        assert_eq!(partial.observed_ms, 300_000);
        assert_eq!(partial.total_wh, 8.0 / 12.0);
        for watts in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(
                energy(&[first.clone(), point(60_000, 60_000, watts, true)])
                    .bars
                    .is_empty()
            );
        }
        assert!(
            energy(&[first, point(60_000, 60_000, 8.0, false)])
                .bars
                .is_empty()
        );
        let zero = energy(&[point(0, 0, 0.0, false), point(60_000, 60_000, 0.0, true)]);
        assert_eq!(zero.bars.len(), 1);
        assert_eq!(zero.total_wh, 0.0);
        assert!(energy(&[]).bars.is_empty());
    }
    #[test]
    fn forecast_uses_actual_limits_and_rejects_unbounded_estimates() {
        let mut battery = BatteryState {
            available: true,
            charging: true,
            plugged: true,
            percentage: 60,
            time_to_full_seconds: 3600,
            ..Default::default()
        };
        battery.protection.enabled = true;
        battery.protection.end_percent = Some(80);
        assert_eq!(forecast(&battery).seconds, 1800.0);
        for seconds in [0, 86401, 234972, u64::MAX] {
            battery.time_to_full_seconds = seconds;
            assert!(forecast(&battery).estimating);
        }
        battery.time_to_full_seconds = 86400;
        assert_eq!(forecast(&battery).seconds, 43200.0);
        battery.protection.charge_once_active = true;
        assert_eq!(forecast(&battery).limit, None);
        battery.protection.charge_once_active = false;
        battery.percentage = 88;
        battery.charging = false;
        assert_eq!(forecast(&battery).status, "limit-reached");
        battery.plugged = false;
        battery.time_to_empty_seconds = 12600;
        let value = forecast(&battery);
        assert_eq!(
            (value.target, value.seconds, value.status.as_str()),
            (0, 12600.0, "valid")
        );
        assert_eq!(value.limit, Some(80));
        for seconds in [0, 604801, u64::MAX] {
            battery.time_to_empty_seconds = seconds;
            assert!(forecast(&battery).estimating);
            assert_eq!(forecast(&battery).seconds, 0.0);
        }
        battery.time_to_empty_seconds = 12600;
        battery.plugged = true;
        assert_eq!(forecast(&battery).seconds, 0.0);
        assert_eq!(forecast(&battery).status, "limit-reached");
        battery.protection.enabled = false;
        assert_eq!(forecast(&battery).status, "unavailable");
        battery.percentage = 100;
        assert_eq!(forecast(&battery).status, "full");
        battery.available = false;
        assert_eq!(forecast(&battery).status, "unavailable");
        battery.available = true;
        battery.plugged = false;
        battery.percentage = 0;
        assert_eq!(forecast(&battery).status, "unavailable");
    }
}
