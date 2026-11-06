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
