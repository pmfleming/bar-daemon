//! Track acceptance separately from the result of the systemd sleep service.
//! Never infer successful sleep from a D-Bus reply, and never replay an action.
use crate::state::StateStore;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Operation {
    pub id: u64,
    pub action: String,
    pub phase: String,
    pub job: Option<String>,
    pub error: Option<String>,
}

pub(super) struct Tracker {
    state: watch::Sender<Operation>,
    started: std::sync::Mutex<Instant>,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            state: watch::channel(Operation::default()).0,
            started: std::sync::Mutex::new(Instant::now()),
        }
    }
}
pub(super) struct CancellationGuard<'a> {
    tracker: &'a Tracker,
    id: u64,
}
impl Drop for CancellationGuard<'_> {
    fn drop(&mut self) {
        let state = self.tracker.state.borrow().clone();
        if state.id != self.id {
            return;
        }
        if state.phase == "requested" {
            self.tracker
                .failed("Sleep request cancelled before dispatch".into(), false);
        } else if state.phase == "dispatching" {
            self.tracker.failed(
                "Sleep dispatch was interrupted; outcome unknown".into(),
                true,
            );
        }
    }
}

pub(super) static TRACKER: LazyLock<Tracker> = LazyLock::new(Tracker::default);
pub(super) fn current() -> Operation {
    TRACKER.state.borrow().clone()
}

fn unfinished(phase: &str) -> bool {
    matches!(
        phase,
        "requested" | "dispatching" | "accepted" | "preparing" | "returned" | "unknown"
    )
}
fn service(action: &str) -> &str {
    match action {
        "suspend" => "systemd-suspend.service",
        "hibernate" => "systemd-hibernate.service",
        "suspend-then-hibernate" => "systemd-suspend-then-hibernate.service",
        _ => "",
    }
}
impl Tracker {
    pub(super) fn cancellation_guard(&self) -> CancellationGuard<'_> {
        CancellationGuard {
            tracker: self,
            id: self.state.borrow().id,
        }
    }
    pub(super) fn begin(&self, action: &str) {
        *self.started.lock().unwrap() = Instant::now();
        self.state.send_modify(|s| {
            *s = Operation {
                id: s.id.saturating_add(1),
                action: action.into(),
                phase: "requested".into(),
                ..Default::default()
            }
        });
    }
    pub(super) fn dispatching(&self) {
        self.state.send_modify(|s| s.phase = "dispatching".into());
    }
    pub(super) fn accepted(&self) {
        self.state.send_modify(|s| {
            if matches!(s.phase.as_str(), "requested" | "dispatching") {
                s.phase = "accepted".into();
            }
        });
    }
    pub(super) fn failed(&self, error: String, ambiguous: bool) {
        self.state.send_modify(|s| {
            // A correlated terminal result outranks a lost method reply.
            if ambiguous && matches!(s.phase.as_str(), "completed" | "failed") {
                return;
            }
            s.phase = if ambiguous { "unknown" } else { "failed" }.into();
            s.error = Some(error);
        });
    }
    pub(super) fn finish_preflight_error(&self, error: &anyhow::Error) {
        if self.state.borrow().phase == "requested" {
            self.failed(format!("{error:#}"), false);
        }
    }
    pub(super) fn prepared(&self, preparing: bool) {
        self.state.send_if_modified(|s| {
            if !unfinished(&s.phase) || s.phase == "requested" {
                return false;
            }
            s.phase = if preparing { "preparing" } else { "returned" }.into();
            true
        });
    }
    fn job_new(&self, path: &str, unit: &str) {
        self.state.send_if_modified(|s| {
            if !unfinished(&s.phase)
                || s.phase == "requested"
                || s.job.is_some()
                || service(&s.action) != unit
            {
                return false;
            }
            s.job = Some(path.into());
            true
        });
    }
    fn job_removed(&self, path: &str, unit: &str, result: &str) {
        self.state.send_if_modified(|s| {
            if s.job.as_deref() != Some(path) || service(&s.action) != unit || !unfinished(&s.phase)
            {
                return false;
            }
            if result == "done" {
                s.phase = "completed".into();
                s.error = None;
            } else {
                s.phase = "failed".into();
                s.error = Some(format!(
                    "{unit}: {result}. Inspect this unit's journal for the sleep failure."
                ));
            }
            true
        });
    }
    fn lost_monitor(&self, reason: &str) {
        self.state.send_if_modified(|s| {
            if !unfinished(&s.phase) || matches!(s.phase.as_str(), "requested" | "unknown") {
                return false;
            }
            s.phase = "unknown".into();
            s.error = Some(reason.into());
            true
        });
    }
    fn expire(&self) {
        if self.started.lock().unwrap().elapsed() >= Duration::from_secs(120) {
            self.lost_monitor("No correlated sleep-job result was received. Outcome unknown; inspect the session before another request.");
        }
    }
}

pub(super) async fn monitor(store: StateStore) {
    tokio::join!(
        async {
            let mut updates = TRACKER.state.subscribe();
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = timer.tick() => TRACKER.expire(),
                    changed = updates.changed() => { if changed.is_err() { return; } }
                }
                let value = updates.borrow_and_update().clone();
                store.record_sleep_operation(value).await;
            }
        },
        async {
            loop {
                if let Err(error) = connected().await {
                    TRACKER.lost_monitor(&format!("Sleep outcome monitor unavailable: {error:#}"));
                    tracing::warn!(%error, "sleep outcome monitor reconnecting");
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    );
}

async fn connected() -> Result<()> {
    let connection = super::system_bus().await?;
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    )
    .await?;
    // One ordered stream: separate streams can deliver JobRemoved before JobNew.
    let mut jobs = manager.receive_all_signals().await?;
    let mut owners = manager.receive_owner_changed().await?;
    let _: () = manager.call("Subscribe", &()).await?;
    loop {
        tokio::select! {
            _ = owners.next() => bail!("systemd owner changed"),
            signal = jobs.next() => {
                let signal = signal.context("job stream ended")?;
                match signal.header().member().map(|m| m.as_str()) {
                    Some("JobNew") => {
                        let (_, path, unit): (u32, zvariant::OwnedObjectPath, String) = signal.body().deserialize()?;
                        TRACKER.job_new(path.as_str(), &unit);
                    }
                    Some("JobRemoved") => {
                        let (_, path, unit, result): (u32, zvariant::OwnedObjectPath, String, String) = signal.body().deserialize()?;
                        TRACKER.job_removed(path.as_str(), &unit, &result);
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_distinguishes_unsent_from_dispatched_work() {
        let t = Tracker::default();
        t.begin("suspend");
        drop(t.cancellation_guard());
        assert_eq!(t.state.borrow().phase, "failed");
        t.begin("suspend");
        let guard = t.cancellation_guard();
        t.dispatching();
        drop(guard);
        assert_eq!(t.state.borrow().phase, "unknown");
    }

    #[test]
    fn acceptance_and_prepare_false_are_not_completion_and_late_failure_is_visible() {
        let t = Tracker::default();
        t.begin("hibernate");
        t.dispatching();
        t.accepted();
        t.job_new("/job/7", "systemd-hibernate.service");
        t.prepared(true);
        t.prepared(false);
        assert_eq!(t.state.borrow().phase, "returned");
        t.job_removed("/job/7", "systemd-hibernate.service", "failed");
        assert_eq!(t.state.borrow().phase, "failed");
        assert!(t.state.borrow().error.as_ref().unwrap().contains("journal"));
        t.accepted();
        assert_eq!(t.state.borrow().phase, "failed");
    }
    #[test]
    fn unrelated_or_old_jobs_never_complete_a_new_operation() {
        let t = Tracker::default();
        t.begin("suspend");
        t.dispatching();
        t.job_new("/job/1", "systemd-hibernate.service");
        assert!(t.state.borrow().job.is_none());
        t.job_new("/job/2", "systemd-suspend.service");
        t.begin("suspend");
        t.dispatching();
        t.accepted();
        t.job_removed("/job/2", "systemd-suspend.service", "done");
        assert_eq!(t.state.borrow().phase, "accepted");
        t.lost_monitor("disconnected");
        assert_eq!(t.state.borrow().phase, "unknown");
        t.job_new("/job/3", "systemd-suspend.service");
        t.job_removed("/job/3", "systemd-suspend.service", "done");
        t.failed("lost reply".into(), true);
        assert_eq!(t.state.borrow().phase, "completed");
    }
}
