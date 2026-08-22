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
            self.initialized = true;
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
        self.warning_sent = state.warning;
        self.critical_sent = state.critical;
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
        if state.critical && !std::mem::replace(&mut self.critical_sent, true) {
            self.warning_sent = true;
            return Some(BatteryAlert::Critical);
        }
        if state.warning && !std::mem::replace(&mut self.warning_sent, true) {
            return Some(BatteryAlert::Warning);
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
    fn reports_full_only_after_initial_state() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(100, true), true), None);
        assert_eq!(tracker.observe(&state(99, true), true), None);
        assert_eq!(
            tracker.observe(&state(100, true), true),
            Some(BatteryAlert::ChargeComplete(100))
        );
    }

    #[test]
    fn full_notification_can_be_disabled() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(99, true), false), None);
        assert_eq!(tracker.observe(&state(100, true), false), None);
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
