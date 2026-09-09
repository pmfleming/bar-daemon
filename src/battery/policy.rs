use super::levels::recovered;
use crate::{
    activity::notifications::service::{NotificationSink, internal_notification},
    model::BatteryState,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum BatteryAlert {
    Warning,
    Critical,
    ChargeComplete(u8),
}

#[derive(Default)]
pub(super) struct AlertTracker {
    initialized: bool,
    warning_sent: bool,
    critical_sent: bool,
    full_sent: bool,
}

impl AlertTracker {
    pub(super) fn observe(
        &mut self,
        state: &BatteryState,
        notify_when_full: bool,
    ) -> Option<BatteryAlert> {
        if !state.available {
            // Wait for the first real reading before suppressing startup alerts.
            return None;
        }
        let complete_at = charge_complete_percent(state);
        if !self.initialized {
            self.initialize(state, complete_at);
            return None;
        }
        if state.plugged {
            self.observe_charging(state.percentage, complete_at, notify_when_full)
        } else {
            self.observe_discharging(state)
        }
    }

    fn initialize(&mut self, state: &BatteryState, complete_at: u8) {
        self.initialized = true;
        self.warning_sent = !state.plugged && state.percentage <= state.policy.warning_percent;
        self.critical_sent = !state.plugged && state.percentage <= state.policy.critical_percent;
        self.full_sent = state.plugged && state.percentage >= complete_at;
    }

    fn observe_charging(
        &mut self,
        percentage: u8,
        complete_at: u8,
        notify_when_full: bool,
    ) -> Option<BatteryAlert> {
        self.warning_sent = false;
        self.critical_sent = false;
        if percentage < complete_at {
            self.full_sent = false;
            return None;
        }
        if std::mem::replace(&mut self.full_sent, true) {
            return None;
        }
        notify_when_full.then_some(BatteryAlert::ChargeComplete(complete_at))
    }

    fn observe_discharging(&mut self, state: &BatteryState) -> Option<BatteryAlert> {
        self.full_sent = false;
        if recovered(state.percentage, state.policy.warning_percent) {
            self.warning_sent = false;
        }
        if recovered(state.percentage, state.policy.critical_percent) {
            self.critical_sent = false;
        }
        if state.percentage <= state.policy.critical_percent {
            // Crossing both levels in one reading emits only the critical alert.
            self.warning_sent = true;
            if !std::mem::replace(&mut self.critical_sent, true) {
                return state
                    .policy
                    .notify_critical
                    .then_some(BatteryAlert::Critical);
            }
        } else if state.percentage <= state.policy.warning_percent
            && !std::mem::replace(&mut self.warning_sent, true)
        {
            return state.policy.notify_warning.then_some(BatteryAlert::Warning);
        }
        None
    }
}

fn charge_complete_percent(state: &BatteryState) -> u8 {
    if state.protection.charge_once_active {
        100
    } else if state.protection.enabled {
        state.protection.end_percent.unwrap_or(100)
    } else {
        100
    }
}

pub(super) async fn send_notification(alert: BatteryAlert, notifications: NotificationSink) {
    let (icon, summary, body, urgency) = match alert {
        BatteryAlert::Warning => (
            "battery-caution",
            "Battery low",
            "Plug in soon.".into(),
            1u8,
        ),
        BatteryAlert::Critical => (
            "battery-empty",
            "Battery critical",
            "Plug in now.".into(),
            2u8,
        ),
        BatteryAlert::ChargeComplete(percent) => (
            "battery-full-charged",
            if percent < 100 {
                "Charge limit reached"
            } else {
                "Battery full"
            },
            if percent < 100 {
                format!("Charging paused at the protected limit of {percent}%.")
            } else {
                "Unplug to reduce wear.".into()
            },
            0u8,
        ),
    };
    if let Err(error) = notifications
        .send(internal_notification(icon, summary, &body, urgency))
        .await
    {
        tracing::warn!(%error, "battery notification failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{AlertTracker, BatteryAlert};
    use crate::model::BatteryState;

    fn state(percent: u8, plugged: bool) -> BatteryState {
        BatteryState {
            available: true,
            percentage: percent,
            plugged,
            warning: !plugged && percent <= 25,
            critical: !plugged && percent <= 12,
            ..BatteryState::default()
        }
    }

    #[test]
    fn alerts_once_per_discharge_cycle_without_startup_noise() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(50, false), true), None);
        assert_eq!(
            tracker.observe(&state(25, false), true),
            Some(BatteryAlert::Warning)
        );
        assert_eq!(tracker.observe(&state(20, false), true), None);
        assert_eq!(
            tracker.observe(&state(12, false), true),
            Some(BatteryAlert::Critical)
        );
        assert_eq!(tracker.observe(&state(10, false), true), None);
        assert_eq!(tracker.observe(&state(50, true), true), None);
        assert_eq!(
            tracker.observe(&state(25, false), true),
            Some(BatteryAlert::Warning)
        );
    }

    #[test]
    fn alerts_rearm_only_above_recovery_margin_and_disabled_alerts_stay_consumed() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(50, false), true), None);
        let mut disabled = state(25, false);
        disabled.policy.notify_warning = false;
        assert_eq!(tracker.observe(&disabled, true), None);
        assert_eq!(tracker.observe(&state(25, false), true), None);
        for percent in [26, 28, 25] {
            assert_eq!(tracker.observe(&state(percent, false), true), None);
        }
        assert_eq!(tracker.observe(&state(29, false), true), None);
        assert_eq!(
            tracker.observe(&state(25, false), true),
            Some(BatteryAlert::Warning)
        );
        assert_eq!(
            tracker.observe(&state(12, false), true),
            Some(BatteryAlert::Critical)
        );
        assert_eq!(tracker.observe(&state(15, false), true), None);
        assert_eq!(tracker.observe(&state(12, false), true), None);
        assert_eq!(tracker.observe(&state(16, false), true), None);
        assert_eq!(
            tracker.observe(&state(12, false), true),
            Some(BatteryAlert::Critical)
        );
    }

    #[test]
    fn startup_waits_for_real_telemetry_and_skips_duplicate_alerts() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&BatteryState::default(), true), None);
        assert_eq!(tracker.observe(&state(10, false), true), None);
        assert_eq!(tracker.observe(&state(9, false), true), None);
        assert_eq!(tracker.observe(&state(50, true), true), None);
        let mut critical = state(10, false);
        critical.policy.notify_critical = false;
        assert_eq!(tracker.observe(&critical, true), None);
        assert_eq!(tracker.observe(&state(9, false), true), None);
        assert_eq!(tracker.observe(&state(20, false), true), None);
    }

    #[test]
    fn protected_limit_counts_as_charge_complete() {
        let mut tracker = AlertTracker::default();
        let mut below = state(79, true);
        below.protection.enabled = true;
        below.protection.end_percent = Some(80);
        assert_eq!(tracker.observe(&below, true), None);
        let mut complete = state(80, true);
        complete.protection.enabled = true;
        complete.protection.end_percent = Some(80);
        assert_eq!(
            tracker.observe(&complete, true),
            Some(BatteryAlert::ChargeComplete(80))
        );
    }
}
