use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::BatteryState;

use super::{CRITICAL_PERCENT, WARNING_PERCENT};

pub(super) fn read_state() -> Result<BatteryState> {
    let root = Path::new("/sys/class/power_supply");
    let battery = std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            std::fs::read_to_string(path.join("type")).is_ok_and(|value| value.trim() == "Battery")
        })
        .context("no system battery is available")?;
    read_battery(&battery)
}

fn read_battery(battery: &Path) -> Result<BatteryState> {
    let native_path = battery
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let percentage = read_u64(&battery.join("capacity")).unwrap_or(0).min(100) as u8;
    let raw_status =
        std::fs::read_to_string(battery.join("status")).unwrap_or_else(|_| "Unknown".into());
    let status = raw_status.trim();
    let charging = status.eq_ignore_ascii_case("charging");
    let discharging = status.eq_ignore_ascii_case("discharging");
    let plugged = !discharging;
    let power_microwatts = read_u64(&battery.join("power_now"))
        .or_else(|| {
            let voltage = read_u64(&battery.join("voltage_now"))?;
            let current = read_u64(&battery.join("current_now"))?;
            Some((u128::from(voltage) * u128::from(current) / 1_000_000) as u64)
        })
        .unwrap_or(0);
    let energy_now = read_u64(&battery.join("energy_now"));
    let energy_full = read_u64(&battery.join("energy_full"));
    let time_to_empty_seconds = if discharging {
        estimate_seconds(energy_now.unwrap_or(0), power_microwatts)
    } else {
        0
    };
    let time_to_full_seconds = if charging {
        estimate_seconds(
            energy_full
                .unwrap_or(0)
                .saturating_sub(energy_now.unwrap_or(0)),
            power_microwatts,
        )
    } else {
        0
    };
    let health_percent = match (energy_full, read_u64(&battery.join("energy_full_design"))) {
        (Some(full), Some(design)) if design > 0 => Some(
            ((u128::from(full) * 100 + u128::from(design) / 2) / u128::from(design)).min(100) as u8,
        ),
        _ => None,
    };
    Ok(BatteryState {
        available: true,
        native_path: native_path.clone(),
        percentage,
        state: status.to_lowercase().replace(' ', "-"),
        charging,
        plugged,
        power_watts: power_microwatts as f64 / 1_000_000.0,
        time_to_empty_seconds,
        time_to_full_seconds,
        health_percent,
        cycles: read_cycle_count(&native_path),
        warning: discharging && percentage <= WARNING_PERCENT,
        critical: discharging && percentage <= CRITICAL_PERCENT,
        error: None,
    })
}

pub(super) fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub(super) fn estimate_seconds(energy_microwatt_hours: u64, power_microwatts: u64) -> u64 {
    if power_microwatts == 0 {
        0
    } else {
        ((u128::from(energy_microwatt_hours) * 3600) / u128::from(power_microwatts))
            .min(u128::from(u64::MAX)) as u64
    }
}

pub(super) fn read_cycle_count(native_path: &str) -> Option<u32> {
    if native_path.is_empty() {
        return None;
    }
    let path = PathBuf::from("/sys/class/power_supply")
        .join(native_path)
        .join("cycle_count");
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::estimate_seconds;

    #[test]
    fn estimates_remaining_seconds_from_sysfs_units() {
        assert_eq!(estimate_seconds(40_000_000, 10_000_000), 14_400);
        assert_eq!(estimate_seconds(40_000_000, 0), 0);
    }
}
