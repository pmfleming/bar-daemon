//! Shared hysteresis for battery notifications and automatic profile actions.
use crate::model::{BatteryProfileAction, BatteryState};
use serde::{Deserialize, Serialize};

pub(crate) const RECOVERY_MARGIN: u8 = 3;

pub(crate) fn recovered(percentage: u8, threshold: u8) -> bool {
    percentage > threshold.saturating_add(RECOVERY_MARGIN).min(100)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BatteryLevel {
    #[default]
    Normal,
    Low,
    Critical,
}

impl BatteryLevel {
    pub(crate) fn observe(self, battery: &BatteryState) -> Self {
        if !battery.available || battery.plugged {
            return Self::Normal;
        }
        let percent = battery.percentage;
        let policy = &battery.policy;
        if percent <= policy.critical_percent
            || (self == Self::Critical && !recovered(percent, policy.critical_percent))
        {
            Self::Critical
        } else if percent <= policy.warning_percent
            || (self != Self::Normal && !recovered(percent, policy.warning_percent))
        {
            Self::Low
        } else {
            Self::Normal
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Low => "low",
            Self::Critical => "critical",
        }
    }

    pub(crate) fn action(self, battery: &BatteryState) -> BatteryProfileAction {
        match self {
            Self::Normal => BatteryProfileAction::KeepCurrent,
            Self::Low => battery.policy.warning_profile,
            Self::Critical => battery.policy.critical_profile,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_enter_at_threshold_and_recover_above_margin() {
        let mut battery = BatteryState {
            available: true,
            percentage: 25,
            ..Default::default()
        };
        let mut level = BatteryLevel::Normal.observe(&battery);
        assert_eq!(level, BatteryLevel::Low);
        for percent in [26, 28, 24] {
            battery.percentage = percent;
            level = level.observe(&battery);
            assert_eq!(level, BatteryLevel::Low);
        }
        battery.percentage = 12;
        level = level.observe(&battery);
        assert_eq!(level, BatteryLevel::Critical);
        battery.percentage = 15;
        assert_eq!(level.observe(&battery), BatteryLevel::Critical);
        battery.percentage = 16;
        level = level.observe(&battery);
        assert_eq!(level, BatteryLevel::Low);
        battery.percentage = 29;
        assert_eq!(level.observe(&battery), BatteryLevel::Normal);
        battery.percentage = 10;
        battery.plugged = true;
        assert_eq!(level.observe(&battery), BatteryLevel::Normal);
        battery.plugged = false;
        battery.available = false;
        assert_eq!(level.observe(&battery), BatteryLevel::Normal);
    }

    #[test]
    fn critical_keep_current_does_not_inherit_low_action() {
        let mut battery = BatteryState {
            available: true,
            percentage: 10,
            ..Default::default()
        };
        battery.policy.critical_profile = BatteryProfileAction::KeepCurrent;
        assert_eq!(
            BatteryLevel::Normal.observe(&battery).action(&battery),
            BatteryProfileAction::KeepCurrent
        );
    }
}
