//! Shared battery helper identity, bounds and validation. No system effects.
use anyhow::{Context, Result, bail};
use std::time::Duration;
pub const BUS_NAME: &str = "org.laufan.BarBatteryHelper";
pub const OBJECT_PATH: &str = "/org/laufan/BarBatteryHelper";
pub const INTERFACE: &str = "org.laufan.BarBatteryHelper1";
pub const AUTHORIZATION_BUDGET: Duration = Duration::from_secs(90);
pub const HELPER_BUDGET: Duration = Duration::from_secs(100);

pub const MAX_MINUTES: u32 = 7 * 24 * 60;
pub fn valid_thresholds(start: u8, end: u8) -> bool {
    start < end && end <= 100
}

pub fn validate_battery_id(battery_id: &str) -> Result<()> {
    let suffix = battery_id
        .strip_prefix("BAT")
        .with_context(|| format!("battery id {battery_id} must start with BAT"))?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("battery id must match BAT followed by digits");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThresholdWriteResult {
    pub actual_start_percent: u8,
    pub actual_end_percent: u8,
    pub verified: bool,
}
