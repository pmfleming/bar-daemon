//! Native unit and duration formatting. Views never reinterpret units or round telemetry.
pub(crate) use shelllist_daemon_core::fixed_decimal;
pub(crate) fn estimate(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "Estimating".into();
    }
    let minutes = (seconds / 60.0).floor() as u64;
    if minutes >= 60 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("{}m", minutes.max(1))
    }
}
pub(crate) fn number(value: f64, suffix: &str) -> String {
    if value.is_finite() {
        format!("{:.0}{suffix}", (value + 0.5).floor() + 0.0)
    } else {
        "—".into()
    }
}
/// Nonnegative telemetry with one decimal place; missing/invalid is not zero.
pub(crate) fn measurement(value: f64, suffix: &str) -> Option<String> {
    (value >= 0.0)
        .then_some(value)
        .and_then(|value| fixed_decimal(value, 1))
        .map(|value| format!("{value}{suffix}"))
}
pub(crate) fn utc_offset(seconds: i32) -> String {
    let minutes = (f64::from(seconds) / 60.0 + 0.5).floor() as i64;
    if minutes == 0 {
        return "UTC".into();
    }
    let sign = if minutes < 0 { "−" } else { "+" };
    let magnitude = minutes.unsigned_abs();
    if magnitude.is_multiple_of(60) {
        format!("UTC{sign}{}", magnitude / 60)
    } else {
        format!("UTC{sign}{}:{:02}", magnitude / 60, magnitude % 60)
    }
}
pub(crate) fn battery(state: &mut crate::model::BatteryState) {
    use serde_json::json;
    let energy = |value: Option<f64>| {
        value
            .and_then(|v| measurement(v, " Wh"))
            .unwrap_or_else(|| "Unknown".into())
    };
    state.display = json!({"percentage":if state.available {format!("{}%",state.percentage)} else {"Unavailable".into()},
        "power":(state.available && state.power_available).then(|| measurement(state.power_watts," W")).flatten().unwrap_or_else(|| "— W".into())});
    for device in &mut state.devices {
        let protection = &device.protection;
        device.display = json!({"energy_now":energy(device.energy_now_wh),"energy_full":energy(device.energy_full_wh),"energy_design":energy(device.energy_full_design_wh),
            "desired_range":protection.desired_start_percent.zip(protection.desired_end_percent).map(|(start,end)| format!("{start}–{end}%")).unwrap_or_else(||"Not configured".into())});
    }
}

#[cfg(test)]
mod tests {
    use super::{battery, estimate, measurement, number, utc_offset};
    #[test]
    fn battery_units_keep_unknown_and_valid_zero_distinct() {
        use crate::model::{BatteryDeviceState, BatteryState};
        let mut state = BatteryState {
            available: true,
            power_available: true,
            power_watts: 0.0,
            percentage: 42,
            devices: vec![BatteryDeviceState {
                energy_now_wh: Some(0.0),
                energy_full_wh: Some(50.25),
                ..Default::default()
            }],
            ..Default::default()
        };
        battery(&mut state);
        assert_eq!(state.display["percentage"], "42%");
        assert_eq!(state.display["power"], "0.0 W");
        assert_eq!(state.devices[0].display["energy_now"], "0.0 Wh");
        assert_eq!(state.devices[0].display["energy_full"], "50.3 Wh");
        assert_eq!(state.devices[0].display["energy_design"], "Unknown");
        for (available, power_available, watts) in [
            (false, true, 0.0),
            (true, false, 0.0),
            (true, true, -1.0),
            (true, true, f64::NAN),
            (true, true, f64::INFINITY),
        ] {
            state.available = available;
            state.power_available = power_available;
            state.power_watts = watts;
            battery(&mut state);
            assert_eq!(state.display["power"], "— W");
            assert_eq!(
                state.display["percentage"],
                if available { "42%" } else { "Unavailable" }
            );
        }
        assert_eq!(measurement(50.25, " Wh").as_deref(), Some("50.3 Wh"));
        for value in [-1.0, f64::NAN, f64::NEG_INFINITY, f64::INFINITY] {
            assert_eq!(measurement(value, " W"), None);
        }
    }
    #[test]
    fn labels_preserve_estimate_rounding_and_fractional_offsets() {
        assert_eq!(estimate(0.0), "Estimating");
        assert_eq!(estimate(f64::NAN), "Estimating");
        assert_eq!(estimate(1.0), "1m");
        assert_eq!(estimate(3599.0), "59m");
        assert_eq!(estimate(3661.0), "1h 1m");
        assert_eq!(number(-1.5, "°"), "-1°");
        assert_eq!(number(-0.5, "°"), "0°");
        assert_eq!(number(f64::NAN, "°"), "—");
        assert_eq!(utc_offset(20700), "UTC+5:45");
        assert_eq!(utc_offset(-12600), "UTC−3:30");
        assert_eq!(utc_offset(0), "UTC");
    }
}
