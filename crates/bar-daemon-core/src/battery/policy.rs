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
    retry_not_before: Option<tokio::time::Instant>,
}

impl AlertTracker {
    pub(super) fn observe(
        &mut self,
        state: &BatteryState,
        notify_when_full: bool,
    ) -> Option<BatteryAlert> {
        if self
            .retry_not_before
            .is_some_and(|deadline| tokio::time::Instant::now() < deadline)
        {
            return None;
        }
        self.retry_not_before = None;
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

    pub(super) async fn deliver(
        &mut self,
        alert: BatteryAlert,
        delivery: impl std::future::Future<Output = anyhow::Result<()>>,
    ) -> anyhow::Result<()> {
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), delivery)
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("battery notification delivery timed out")));
        if result.is_err() {
            match alert {
                BatteryAlert::Warning => self.warning_sent = false,
                BatteryAlert::Critical => self.critical_sent = false,
                BatteryAlert::ChargeComplete(_) => self.full_sent = false,
            }
            self.retry_not_before =
                Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5));
        }
        result
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

pub(super) async fn send_notification(
    alert: BatteryAlert,
    notifications: NotificationSink,
) -> anyhow::Result<()> {
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
    notifications
        .send(internal_notification(icon, summary, &body, urgency))
        .await?;
    Ok(())
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

    #[tokio::test]
    async fn failed_bus_delivery_retries_only_current_alert_and_acknowledgement_consumes_it() {
        let bus = crate::battery::helper::authorization_tests::Bus::new();
        let connection = bus.connection().await;
        let mut tracker = AlertTracker::default();
        tracker.observe(&state(50, false), true);
        let alert = tracker.observe(&state(12, false), true).unwrap();
        let result = tracker
            .deliver(alert, async {
                crate::activity::notifications::service::send_freedesktop_on(
                    &connection,
                    crate::activity::notifications::service::internal_notification(
                        "battery", "Critical", "Low", 2,
                    ),
                )
                .await?;
                Ok(())
            })
            .await;
        assert!(result.is_err());
        assert_eq!(
            tracker.observe(&state(12, false), true),
            None,
            "failed sends back off"
        );
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        assert_eq!(
            tracker.observe(&state(12, false), true),
            Some(BatteryAlert::Critical)
        );
        tracker.deliver(alert, async { Ok(()) }).await.unwrap();
        assert_eq!(
            tracker.observe(&state(12, false), true),
            None,
            "acknowledged alerts never replay"
        );
        tracker.observe(&state(50, true), true);
        assert_eq!(
            tracker.observe(&state(12, false), true),
            Some(BatteryAlert::Critical)
        );
    }

    #[test]
    fn alerts_rearm_only_above_recovery_margin_and_disabled_alerts_stay_consumed() {
        let mut tracker = AlertTracker::default();
        assert_eq!(tracker.observe(&BatteryState::default(), true), None);
        assert_eq!(tracker.observe(&state(10, false), true), None);
        assert_eq!(tracker.observe(&state(9, false), true), None);
        assert_eq!(tracker.observe(&state(50, true), true), None);
        let mut disabled = state(25, false);
        disabled.policy.notify_warning = false;
        assert_eq!(tracker.observe(&disabled, true), None);
        for (percent, plugged, expected) in [
            (25, false, None),
            (26, false, None),
            (28, false, None),
            (25, false, None),
            (29, false, None),
            (25, false, Some(BatteryAlert::Warning)),
            (12, false, Some(BatteryAlert::Critical)),
            (15, false, None),
            (12, false, None),
            (16, false, None),
            (12, false, Some(BatteryAlert::Critical)),
            (10, false, None),
            (50, true, None),
            (25, false, Some(BatteryAlert::Warning)),
        ] {
            assert_eq!(
                tracker.observe(&state(percent, plugged), true),
                expected,
                "{percent}% plugged={plugged}"
            );
        }
        let mut critical = state(10, false);
        critical.policy.notify_critical = false;
        assert_eq!(tracker.observe(&critical, true), None);
        assert_eq!(tracker.observe(&state(9, false), true), None);
        assert_eq!(tracker.observe(&state(20, false), true), None);
        let mut charging = state(79, true);
        charging.protection.enabled = true;
        charging.protection.end_percent = Some(80);
        assert_eq!(tracker.observe(&charging, true), None);
        charging.percentage = 80;
        assert_eq!(
            tracker.observe(&charging, true),
            Some(BatteryAlert::ChargeComplete(80))
        );
    }
}
