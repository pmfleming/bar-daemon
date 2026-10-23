//! Observed-time windowing and measurement validity belong to the battery domain.
//! QML paints these segments and hit-tests them; it never decides which readings exist.
use crate::model::{BatteryHistoryPoint, BatteryHistoryState};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Point {
    x: f64,
    value: f64,
    timestamp_ms: u64,
    charging: bool,
    label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Break {
    from: Point,
    to: Point,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Series {
    segments: Vec<Vec<Point>>,
    breaks: Vec<Break>,
    maximum: f64,
    axis_maximum: f64,
    tick_labels: Vec<String>,
    active_duration_ms: u64,
    duration_label: String,
    has_active_timeline: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Chart {
    percentage: Series,
    power: Series,
}

fn observed(point: &BatteryHistoryPoint) -> bool {
    point.timestamp_ms > 0
}
fn metric(point: &BatteryHistoryPoint, power: bool) -> Option<f64> {
    let value = if power {
        point.power_watts
    } else {
        f64::from(point.percentage)
    };
    if !observed(point) || !value.is_finite() || value < 0.0 {
        return None;
    }
    if power {
        if point.power_valid == Some(false) || (point.power_valid != Some(true) && value == 0.0) {
            return None;
        }
    } else if value > 100.0 {
        return None;
    }
    Some(value)
}
fn series(points: &[&BatteryHistoryPoint], power: bool) -> Series {
    let first = points.iter().map(|p| p.active_time_ms).min().unwrap_or(0);
    let last = points
        .iter()
        .map(|p| p.active_time_ms)
        .max()
        .unwrap_or(first);
    let duration = last.saturating_sub(first);
    let mut result = Series {
        segments: vec![],
        breaks: vec![],
        maximum: if power { 1.0 } else { 100.0 },
        axis_maximum: 0.0,
        tick_labels: vec![],
        active_duration_ms: duration,
        duration_label: crate::presentation::estimate(duration as f64 / 1000.0),
        has_active_timeline: !points.is_empty(),
    };
    let mut previous: Option<&BatteryHistoryPoint> = None;
    for point in points {
        let Some(value) = metric(point, power) else {
            previous = None;
            continue;
        };
        let next = Point {
            x: if duration > 0 {
                (point.active_time_ms - first) as f64 / duration as f64
            } else {
                0.5
            },
            value,
            timestamp_ms: point.timestamp_ms,
            charging: point.charging,
            label: if power {
                format!(
                    "{} W",
                    crate::presentation::fixed_decimal(value, 1).unwrap()
                )
            } else {
                format!("{value:.0}%")
            },
        };
        let continuous = previous.is_some_and(|old| {
            point.continuous
                && point.active_time_ms > old.active_time_ms
                && point.timestamp_ms > old.timestamp_ms
        });
        if !continuous {
            if previous.is_some() {
                result.breaks.push(Break {
                    from: result.segments.last().unwrap().last().unwrap().clone(),
                    to: next.clone(),
                });
            }
            result.segments.push(vec![]);
        }
        result.segments.last_mut().unwrap().push(next);
        result.maximum = result.maximum.max(value);
        previous = Some(point);
    }
    result.axis_maximum = if power {
        (result.maximum / 10.0).ceil().max(1.0) * 10.0
    } else {
        100.0
    };
    result.tick_labels = vec![
        format!("+{:.0}", result.axis_maximum),
        "0".into(),
        format!("−{:.0}", result.axis_maximum),
    ];
    result
}

pub(crate) fn project(history: &BatteryHistoryState, hours: u16) -> Chart {
    let mut points: Vec<_> = history
        .points
        .iter()
        .filter(|point| observed(point))
        .collect();
    if let Some(current) = history
        .current_point
        .as_ref()
        .filter(|point| observed(point))
        && points
            .last()
            .is_none_or(|last| current.timestamp_ms > last.timestamp_ms)
    {
        points.push(current);
    }
    let last = points
        .iter()
        .map(|point| point.active_time_ms)
        .max()
        .unwrap_or(0);
    let cutoff = last.saturating_sub(u64::from(hours) * 3_600_000);
    points.retain(|point| point.active_time_ms >= cutoff);
    Chart {
        percentage: series(&points, false),
        power: series(&points, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(
        active: u64,
        wall: u64,
        continuous: bool,
        power: f64,
        valid: Option<bool>,
    ) -> BatteryHistoryPoint {
        BatteryHistoryPoint {
            active_time_ms: active,
            timestamp_ms: wall,
            continuous,
            power_watts: power,
            power_valid: valid,
            percentage: 50,
            ..Default::default()
        }
    }
    #[test]
    fn gaps_zero_and_missing_readings_are_distinct() {
        let history = BatteryHistoryState {
            points: vec![
                point(0, 1, false, 0.0, Some(true)),
                point(10, 11, true, 2.0, None),
                point(20, 100, false, 0.0, Some(false)),
                point(30, 110, true, 3.0, Some(true)),
            ],
            ..Default::default()
        };
        let chart = project(&history, 6);
        assert_eq!(
            chart
                .percentage
                .segments
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [2, 2]
        );
        assert_eq!(chart.percentage.breaks.len(), 1);
        assert_eq!(
            chart
                .power
                .segments
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert!(
            chart.power.breaks.is_empty(),
            "missing readings must not be interpolated"
        );
        assert_eq!(chart.power.segments[0][0].value, 0.0);
        assert_eq!(chart.percentage.active_duration_ms, 30);
    }
    #[test]
    fn windows_use_active_time_and_do_not_append_stale_live_samples() {
        let mut history = BatteryHistoryState {
            points: vec![
                point(0, 1, false, 1.0, None),
                point(7 * 3_600_000, 10_000_000, true, 2.0, None),
            ],
            current_point: Some(point(8 * 3_600_000, 10_000_001, true, 3.0, None)),
            ..Default::default()
        };
        let chart = project(&history, 6);
        assert_eq!(chart.power.active_duration_ms, 3_600_000);
        assert_eq!(chart.power.segments[0].len(), 2);
        history.current_point = Some(point(9 * 3_600_000, 9_000_000, true, 3.0, None));
        let chart = project(&history, 6);
        assert_eq!(chart.power.segments[0].len(), 1);
        assert_eq!(chart.power.segments[0][0].x, 0.5);
        history.points.clear();
        history.current_point = None;
        assert!(!project(&history, 6).percentage.has_active_timeline);
    }
}
