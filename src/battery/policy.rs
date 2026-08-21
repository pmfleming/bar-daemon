use crate::{
    activity::notifications::service::{NotificationSink, internal_notification},
    model::BatteryState,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum BatteryAlert {
    Warning,
    Critical,
    Full,
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
        if !self.initialized {
            self.initialized = true;
            self.warning_sent = state.warning;
            self.critical_sent = state.critical;
            self.full_sent = state.plugged && state.percentage >= 100;
            return None;
        }
        if state.plugged {
            self.warning_sent = false;
            self.critical_sent = false;
            if state.percentage < 100 {
                self.full_sent = false;
            }
            if state.percentage >= 100 && !self.full_sent {
                self.full_sent = true;
                return notify_when_full.then_some(BatteryAlert::Full);
            }
            return None;
        }
        self.full_sent = false;
        if state.critical && !self.critical_sent {
            self.critical_sent = true;
            self.warning_sent = true;
            return Some(BatteryAlert::Critical);
        }
        if state.warning && !self.warning_sent {
            self.warning_sent = true;
            return Some(BatteryAlert::Warning);
        }
        None
    }
}

pub(super) async fn send_notification(alert: BatteryAlert, notifications: NotificationSink) {
    let (icon, summary, body, urgency) = match alert {
        BatteryAlert::Warning => ("battery-caution", "Battery low", "Plug in soon.", 1u8),
        BatteryAlert::Critical => ("battery-empty", "Battery critical", "Plug in now.", 2u8),
        BatteryAlert::Full => (
            "battery-full-charged",
            "Battery full",
            "Unplug to reduce wear.",
            0u8,
        ),
    };
    if let Err(error) = notifications
        .send(internal_notification(icon, summary, body, urgency))
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
            Some(BatteryAlert::Full)
        );
    }

    #[test]
    fn full_notification_can_be_disabled() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&state(99, true), false), None);
        assert_eq!(tracker.observe(&state(100, true), false), None);
    }
}
