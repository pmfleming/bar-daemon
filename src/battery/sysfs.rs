use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::{BatteryDeviceState, BatteryProtectionState, BatteryState};

use super::{
    CRITICAL_PERCENT, WARNING_PERCENT,
    model::{NativeBattery, NativeSnapshot},
};

#[derive(Debug, Clone)]
pub(super) struct PowerSupplyFs {
    root: PathBuf,
}

impl PowerSupplyFs {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn system() -> Self {
        Self::new(PathBuf::from("/sys/class/power_supply"))
    }

    pub(super) fn read_snapshot(&self) -> Result<NativeSnapshot> {
        let mut paths = std::fs::read_dir(&self.root)
            .with_context(|| format!("read {}", self.root.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();

        let mut snapshot = NativeSnapshot::default();
        for path in paths {
            let supply_type = read_string(&path.join("type")).unwrap_or_default();
            if supply_type == "Battery" {
                if let Ok(battery) = read_battery(&path) {
                    snapshot.batteries.push(battery);
                }
            } else if read_u64(&path.join("online")) == Some(1) {
                snapshot.plugged = true;
            }
        }
        if snapshot.batteries.is_empty() {
            anyhow::bail!("no system battery is available");
        }
        Ok(snapshot)
    }

    pub(super) fn read_state(&self) -> Result<BatteryState> {
        Ok(aggregate(&self.read_snapshot()?))
    }
}

fn read_battery(path: &Path) -> Result<NativeBattery> {
    let id = path
        .file_name()
        .context("battery path has no name")?
        .to_string_lossy()
        .to_string();
    let present = read_u64(&path.join("present")).unwrap_or(1) != 0;
    let voltage_uv = read_u64(&path.join("voltage_now"));
    let energy_now_uwh = read_energy(path, "energy_now", "charge_now", voltage_uv);
    let energy_full_uwh = read_energy(path, "energy_full", "charge_full", voltage_uv);
    let energy_full_design_uwh =
        read_energy(path, "energy_full_design", "charge_full_design", voltage_uv);
    let percentage = read_u64(&path.join("capacity"))
        .map(|value| value.min(100) as u8)
        .or_else(|| ratio_percent(energy_now_uwh, energy_full_uwh))
        .unwrap_or(0);
    let power_uw = read_u64(&path.join("power_now"))
        .or_else(|| multiply_micro(read_u64(&path.join("current_now"))?, voltage_uv?))
        .unwrap_or(0);
    let (charge_behaviour, available_behaviours) = read_choices(&path.join("charge_behaviour"));

    Ok(NativeBattery {
        id,
        vendor: read_string(&path.join("manufacturer")).unwrap_or_default(),
        model: read_string(&path.join("model_name")).unwrap_or_default(),
        serial: read_string(&path.join("serial_number")).unwrap_or_default(),
        present,
        status: read_string(&path.join("status")).unwrap_or_else(|| "Unknown".into()),
        percentage,
        energy_now_uwh,
        energy_full_uwh,
        energy_full_design_uwh,
        power_uw,
        voltage_uv,
        cycles: read_u64(&path.join("cycle_count")).and_then(|value| value.try_into().ok()),
        start_threshold: read_percent(&path.join("charge_control_start_threshold")),
        end_threshold: read_percent(&path.join("charge_control_end_threshold")),
        charge_behaviour,
        available_behaviours,
    })
}

fn aggregate(snapshot: &NativeSnapshot) -> BatteryState {
    let present = snapshot
        .batteries
        .iter()
        .filter(|battery| battery.present)
        .collect::<Vec<_>>();
    let all_energy = present
        .iter()
        .all(|battery| battery.energy_now_uwh.is_some() && battery.energy_full_uwh.is_some());
    let energy_now = all_energy.then(|| {
        present
            .iter()
            .filter_map(|battery| battery.energy_now_uwh)
            .sum::<u64>()
    });
    let energy_full = all_energy.then(|| {
        present
            .iter()
            .filter_map(|battery| battery.energy_full_uwh)
            .sum::<u64>()
    });
    let percentage = ratio_percent(energy_now, energy_full).unwrap_or_else(|| {
        if present.is_empty() {
            0
        } else {
            let total = present
                .iter()
                .map(|battery| u64::from(battery.percentage))
                .sum::<u64>();
            ((total + present.len() as u64 / 2) / present.len() as u64).min(100) as u8
        }
    });
    let charging = present.iter().any(|battery| battery.charging());
    let discharging = present.iter().any(|battery| battery.discharging());
    let state = if charging {
        "charging"
    } else if discharging {
        "discharging"
    } else if !present.is_empty() && present.iter().all(|battery| battery.fully_charged()) {
        "fully-charged"
    } else {
        "not-charging"
    };
    let power_uw = present.iter().map(|battery| battery.power_uw).sum::<u64>();
    let design_total = present
        .iter()
        .map(|battery| battery.energy_full_design_uwh)
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum::<u64>());
    let health_percent = ratio_percent(energy_full, design_total);
    let time_to_empty_seconds = if discharging {
        estimate_seconds(energy_now.unwrap_or(0), power_uw)
    } else {
        0
    };
    let time_to_full_seconds = if charging {
        estimate_seconds(
            energy_full
                .unwrap_or(0)
                .saturating_sub(energy_now.unwrap_or(0)),
            power_uw,
        )
    } else {
        0
    };
    let devices = present
        .iter()
        .map(|battery| device_state(battery))
        .collect::<Vec<_>>();
    let protection = devices
        .first()
        .map(|device| device.protection.clone())
        .unwrap_or_default();

    BatteryState {
        available: !present.is_empty(),
        native_path: present
            .first()
            .map_or_else(String::new, |battery| battery.id.clone()),
        percentage,
        state: state.into(),
        charging,
        plugged: snapshot.plugged,
        power_watts: power_uw as f64 / 1_000_000.0,
        time_to_empty_seconds,
        time_to_full_seconds,
        health_percent,
        cycles: (present.len() == 1).then(|| present[0].cycles).flatten(),
        warning: !snapshot.plugged && percentage <= WARNING_PERCENT,
        critical: !snapshot.plugged && percentage <= CRITICAL_PERCENT,
        protection,
        devices,
        error: None,
    }
}

fn device_state(battery: &NativeBattery) -> BatteryDeviceState {
    let protection = BatteryProtectionState {
        supported: battery.start_threshold.is_some() || battery.end_threshold.is_some(),
        backend: "thinkpad-sysfs".into(),
        enabled: battery.start_threshold.is_some_and(|value| value > 0)
            || battery.end_threshold.is_some_and(|value| value < 100),
        start_percent: battery.start_threshold,
        end_percent: battery.end_threshold,
        supports_start: battery.start_threshold.is_some(),
        supports_end: battery.end_threshold.is_some(),
        supports_charge_behaviour: !battery.available_behaviours.is_empty(),
        charge_behaviour: battery.charge_behaviour.clone(),
        available_behaviours: battery.available_behaviours.clone(),
        error: None,
    };
    BatteryDeviceState {
        id: battery.id.clone(),
        vendor: battery.vendor.clone(),
        model: battery.model.clone(),
        serial: battery.serial.clone(),
        present: battery.present,
        percentage: battery.percentage,
        state: battery.status.to_lowercase().replace(' ', "-"),
        power_watts: battery.power_uw as f64 / 1_000_000.0,
        energy_now_wh: micro_to_base(battery.energy_now_uwh),
        energy_full_wh: micro_to_base(battery.energy_full_uwh),
        energy_full_design_wh: micro_to_base(battery.energy_full_design_uwh),
        voltage_volts: micro_to_base(battery.voltage_uv),
        health_percent: ratio_percent(battery.energy_full_uwh, battery.energy_full_design_uwh),
        cycles: battery.cycles,
        protection,
    }
}

fn micro_to_base(value: Option<u64>) -> Option<f64> {
    value.map(|value| value as f64 / 1_000_000.0)
}

fn read_energy(
    path: &Path,
    energy_name: &str,
    charge_name: &str,
    voltage_uv: Option<u64>,
) -> Option<u64> {
    read_u64(&path.join(energy_name))
        .or_else(|| multiply_micro(read_u64(&path.join(charge_name))?, voltage_uv?))
}

fn multiply_micro(left: u64, right: u64) -> Option<u64> {
    (u128::from(left) * u128::from(right) / 1_000_000)
        .try_into()
        .ok()
}

fn ratio_percent(now: Option<u64>, full: Option<u64>) -> Option<u8> {
    let (Some(now), Some(full)) = (now, full) else {
        return None;
    };
    if full == 0 {
        return None;
    }
    Some(((u128::from(now) * 100 + u128::from(full) / 2) / u128::from(full)).min(100) as u8)
}

pub(super) fn read_cycle_count(native_path: &str) -> Option<u32> {
    if native_path.is_empty() {
        return None;
    }
    read_u64(
        &Path::new("/sys/class/power_supply")
            .join(native_path)
            .join("cycle_count"),
    )
    .and_then(|value| value.try_into().ok())
}

fn read_choices(path: &Path) -> (Option<String>, Vec<String>) {
    let Some(value) = read_string(path) else {
        return (None, Vec::new());
    };
    let mut current = None;
    let choices = value
        .split_whitespace()
        .map(|choice| {
            let selected = choice.starts_with('[') && choice.ends_with(']');
            let choice = choice.trim_matches(['[', ']']).to_string();
            if selected {
                current = Some(choice.clone());
            }
            choice
        })
        .collect();
    (current, choices)
}

fn read_percent(path: &Path) -> Option<u8> {
    read_u64(path).and_then(|value| (value <= 100).then_some(value as u8))
}

fn read_string(path: &Path) -> Option<String> {
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_string(path)?.parse().ok()
}

pub(super) fn estimate_seconds(energy_uwh: u64, power_uw: u64) -> u64 {
    if power_uw == 0 {
        0
    } else {
        ((u128::from(energy_uwh) * 3600) / u128::from(power_uw)).min(u128::from(u64::MAX)) as u64
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::TempDir;

    use super::PowerSupplyFs;

    fn write(root: &Path, supply: &str, name: &str, value: &str) {
        let directory = root.join(supply);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(name), value).unwrap();
    }

    #[test]
    fn detects_external_power_independently_of_battery_status() {
        let directory = TempDir::new().unwrap();
        write(directory.path(), "BAT0", "type", "Battery\n");
        write(directory.path(), "BAT0", "status", "Not charging\n");
        write(directory.path(), "BAT0", "capacity", "80\n");
        write(directory.path(), "AC", "type", "Mains\n");
        write(directory.path(), "AC", "online", "1\n");
        let state = PowerSupplyFs::new(directory.path().into())
            .read_state()
            .unwrap();
        assert!(state.plugged);
        assert_eq!(state.state, "not-charging");
    }

    #[test]
    fn aggregates_batteries_by_energy() {
        let directory = TempDir::new().unwrap();
        for (battery, now, full) in [("BAT0", "100", "200"), ("BAT1", "700", "800")] {
            write(directory.path(), battery, "type", "Battery\n");
            write(directory.path(), battery, "status", "Discharging\n");
            write(directory.path(), battery, "energy_now", now);
            write(directory.path(), battery, "energy_full", full);
        }
        let state = PowerSupplyFs::new(directory.path().into())
            .read_state()
            .unwrap();
        assert_eq!(state.percentage, 80);
        assert_eq!(state.state, "discharging");
    }

    #[test]
    fn converts_charge_and_voltage_to_energy_and_power() {
        let directory = TempDir::new().unwrap();
        write(directory.path(), "BAT0", "type", "Battery\n");
        write(directory.path(), "BAT0", "status", "Discharging\n");
        write(directory.path(), "BAT0", "charge_now", "4000000\n");
        write(directory.path(), "BAT0", "charge_full", "5000000\n");
        write(directory.path(), "BAT0", "voltage_now", "10000000\n");
        write(directory.path(), "BAT0", "current_now", "1000000\n");
        let state = PowerSupplyFs::new(directory.path().into())
            .read_state()
            .unwrap();
        assert_eq!(state.percentage, 80);
        assert_eq!(state.power_watts, 10.0);
        assert_eq!(state.time_to_empty_seconds, 14_400);
    }

    #[test]
    fn exposes_thinkpad_protection_capabilities() {
        let directory = TempDir::new().unwrap();
        write(directory.path(), "BAT0", "type", "Battery\n");
        write(directory.path(), "BAT0", "status", "Not charging\n");
        write(directory.path(), "BAT0", "capacity", "80\n");
        write(
            directory.path(),
            "BAT0",
            "charge_control_start_threshold",
            "75\n",
        );
        write(
            directory.path(),
            "BAT0",
            "charge_control_end_threshold",
            "80\n",
        );
        write(
            directory.path(),
            "BAT0",
            "charge_behaviour",
            "[auto] inhibit-charge force-discharge\n",
        );
        let state = PowerSupplyFs::new(directory.path().into())
            .read_state()
            .unwrap();
        assert!(state.protection.supported);
        assert!(state.protection.enabled);
        assert_eq!(state.protection.start_percent, Some(75));
        assert_eq!(state.protection.charge_behaviour.as_deref(), Some("auto"));
        assert_eq!(state.devices[0].id, "BAT0");
    }
}
