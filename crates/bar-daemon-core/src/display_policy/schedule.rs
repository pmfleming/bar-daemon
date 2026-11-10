//! Events wake reconciliation; the timer is a safety net, not the primary feed.
use super::{DisplayPolicyState, DockingState};
use futures::FutureExt;
use std::time::Duration;
use tokio::sync::Notify;

pub(super) fn interval(state: &DisplayPolicyState) -> Duration {
    if state.layout.trial.is_some()
        || state.docking_state == DockingState::Applying
        || matches!(
            state.status.as_str(),
            "pending" | "settling" | "layout-preview" | "error"
        )
    {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(30)
    }
}

pub(super) async fn wake(changed: &Notify, after: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(after) => false,
        _ = changed.notified() => {
            // A fixed window coalesces paired monitoradded/monitoraddedv2 and
            // bursts without allowing continuous events to postpone recovery.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = changed.notified().now_or_never();
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn events_are_bounded_and_idle_reconciliation_has_a_fallback() {
        let changed = Notify::new();
        let state = DisplayPolicyState {
            status: "external".into(),
            ..Default::default()
        };
        assert_eq!(interval(&state), Duration::from_secs(30));
        let start = tokio::time::Instant::now();
        changed.notify_one();
        let burst = async {
            for _ in 0..9 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                changed.notify_one();
            }
        };
        tokio::join!(wake(&changed, interval(&state)), burst);
        assert_eq!(start.elapsed(), Duration::from_millis(50));
        let start = tokio::time::Instant::now();
        wake(&changed, interval(&state)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(30));
        assert_eq!(
            interval(&DisplayPolicyState {
                status: "layout-preview".into(),
                ..state
            }),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn healthy_idle_gaps_preserve_docking_but_stalls_invalidate_it() {
        let external = super::super::Output {
            name: "DP-1".into(),
            width: 1920,
            height: 1080,
            scale: 1.0,
            ..Default::default()
        };
        let outputs = [external];
        let mut planner = super::super::Planner::default();
        let start = std::time::Instant::now();
        assert_eq!(planner.plan(true, &outputs, start).status, "settling");
        assert_eq!(
            planner
                .plan(true, &outputs, start + Duration::from_secs(5))
                .status,
            "external"
        );
        assert_eq!(
            planner
                .plan(true, &outputs, start + Duration::from_secs(35))
                .status,
            "external"
        );
        assert_eq!(
            planner
                .plan(true, &outputs, start + Duration::from_secs(110))
                .status,
            "settling"
        );
    }
}
