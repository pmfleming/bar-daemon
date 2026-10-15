//! Observation-based docking completion; independent of request acknowledgement.
use super::DisplayPolicyState;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const APPLY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DockingState {
    Applying,
    #[default]
    Settled,
    Failed,
}

#[derive(Default)]
pub(super) struct Transition {
    started: Option<Instant>,
}

impl Transition {
    pub fn update(&mut self, state: &mut DisplayPolicyState, result: Result<&str>, now: Instant) {
        // A confirmed manual layout supersedes automatic enablement. A trial
        // alone does not erase a failed policy: Revert must not retry that policy.
        if state.layout.manual_enablement && state.layout.trial.is_none() {
            state.docking_state = DockingState::Settled;
        }
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                self.fail(state, format!("{error:#}"));
                return;
            }
        };
        if state.docking_state == DockingState::Failed {
            self.started = None;
            return; // Retain the failure until explicit policy retry/manual Keep.
        }
        state.status = status.into();
        state.error = None;
        if status == "sleeping" {
            self.started = None;
            return; // Sleep is not evidence that a submitted policy settled.
        }
        let applying = !state.layout.manual_enablement
            && state.layout.trial.is_none()
            && matches!(status, "pending" | "settling" | "layout-preview");
        if applying {
            let started = *self.started.get_or_insert(now);
            if now.saturating_duration_since(started) >= APPLY_TIMEOUT {
                self.fail(state, "Docking change was not observed within 20 seconds. Save the docking preference again to retry, or preview a manual layout.".into());
            } else {
                state.docking_state = DockingState::Applying;
            }
        } else {
            self.started = None;
            state.docking_state = DockingState::Settled;
        }
    }

    fn fail(&mut self, state: &mut DisplayPolicyState, message: String) {
        self.started = None;
        state.docking_state = DockingState::Failed;
        state.status = "error".into();
        state.error = Some(message);
    }
}
