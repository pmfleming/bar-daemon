//! Track acceptance separately from the result of the systemd sleep service.
//! Never infer successful sleep from a D-Bus reply, and never replay an action.
use crate::{model::SleepOperation, state::StateStore};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::watch;

struct Tracked {
    operation: SleepOperation,
    started: Instant,
    lease: Option<zvariant::OwnedFd>,
}

pub(super) struct Tracker {
    state: watch::Sender<Tracked>,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            state: watch::channel(Tracked {
                operation: SleepOperation::default(),
                started: Instant::now(),
                lease: None,
            })
            .0,
        }
    }
}
pub(super) struct CancellationGuard<'a> {
    tracker: &'a Tracker,
    id: u64,
}
impl Drop for CancellationGuard<'_> {
    fn drop(&mut self) {
        self.tracker.modify(|s| {
            if s.id != self.id {
                return false;
            }
            let (phase, error) = match s.phase.as_str() {
                "requested" => ("failed", "Sleep request cancelled before dispatch"),
                "dispatching" => ("unknown", "Sleep dispatch was interrupted; outcome unknown"),
                _ => return false,
            };
            s.phase = phase.into();
            s.error = Some(error.into());
            true
        });
    }
}

pub(super) static TRACKER: LazyLock<Tracker> = LazyLock::new(Tracker::default);
pub(super) fn current() -> SleepOperation {
    TRACKER.state.borrow().operation.clone()
}

pub(crate) fn retain_lease(fd: zvariant::OwnedFd) -> Result<()> {
    TRACKER.retain_lease(fd)
}

fn dispatched(phase: &str) -> bool {
    matches!(phase, "dispatching" | "accepted" | "preparing" | "returned")
}
fn unfinished(phase: &str) -> bool {
    dispatched(phase) || matches!(phase, "requested" | "unknown")
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
    // Outcome, clock and FD have one owner and one atomic update boundary.
    // Releasing a terminal lease does not depend on the telemetry monitor running.
    fn modify(&self, update: impl FnOnce(&mut SleepOperation) -> bool) {
        self.state.send_if_modified(|state| {
            let changed = update(&mut state.operation);
            if matches!(
                state.operation.phase.as_str(),
                "failed" | "completed" | "unknown"
            ) {
                state.lease = None;
            }
            changed
        });
    }
    fn retain_lease(&self, fd: zvariant::OwnedFd) -> Result<()> {
        anyhow::ensure!(
            self.state.send_if_modified(|state| {
                if state.operation.phase != "requested" || state.lease.is_some() {
                    return false;
                }
                state.lease = Some(fd);
                true
            }),
            "hibernate settings lease requires an unleased, undispatched sleep request"
        );
        Ok(())
    }
    pub(super) fn ready_for_new_request(&self) -> bool {
        let state = self.state.borrow();
        state.operation.phase != "requested" && !dispatched(&state.operation.phase)
    }
    pub(super) fn cancellation_guard(&self) -> CancellationGuard<'_> {
        CancellationGuard {
            tracker: self,
            id: self.state.borrow().operation.id,
        }
    }
    pub(super) fn begin(&self, action: &str) {
        self.state.send_modify(|s| {
            s.operation = SleepOperation {
                id: s.operation.id.saturating_add(1),
                action: action.into(),
                phase: "requested".into(),
                ..Default::default()
            };
            s.started = Instant::now();
            s.lease = None;
        });
    }
    pub(super) fn dispatching(&self) {
        self.modify(|s| {
            s.phase = "dispatching".into();
            true
        });
    }
    pub(super) fn accepted(&self) {
        self.modify(|s| {
            if !matches!(s.phase.as_str(), "requested" | "dispatching") {
                return false;
            }
            s.phase = "accepted".into();
            true
        });
    }
    pub(super) fn failed(&self, error: String, ambiguous: bool) {
        self.modify(|s| {
            // A correlated terminal result outranks a lost method reply.
            if ambiguous && matches!(s.phase.as_str(), "completed" | "failed") {
                return false;
            }
            s.phase = if ambiguous { "unknown" } else { "failed" }.into();
            s.error = Some(error);
            true
        });
    }
    pub(super) fn finish_preflight_error(&self, error: &anyhow::Error) {
        if self.state.borrow().operation.phase == "requested" {
            self.failed(format!("{error:#}"), false);
        }
    }
    pub(super) fn prepared(&self, preparing: bool) {
        self.modify(|s| {
            if !dispatched(&s.phase) {
                return false;
            }
            s.phase = if preparing { "preparing" } else { "returned" }.into();
            true
        });
    }
    fn job_new(&self, path: &str, unit: &str) {
        self.modify(|s| {
            if !dispatched(&s.phase) || s.job.is_some() || service(&s.action) != unit {
                return false;
            }
            s.job = Some(path.into());
            true
        });
    }
    fn job_removed(&self, path: &str, unit: &str, result: &str) {
        self.modify(|s| {
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
        self.modify(|s| {
            if !dispatched(&s.phase) {
                return false;
            }
            s.phase = "unknown".into();
            s.error = Some(reason.into());
            true
        });
    }
    fn expire(&self) {
        if self.state.borrow().started.elapsed() >= Duration::from_secs(120) {
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
                let value = updates.borrow_and_update().operation.clone();
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
    use super::Tracker;
    use std::{
        io::{ErrorKind, Read},
        os::{fd::OwnedFd, unix::net::UnixStream},
    };

    #[test]
    fn terminal_outcomes_release_the_lease_without_a_running_monitor() {
        for result in ["done", "failed", "unknown", "cancelled"] {
            let t = Tracker::default();
            t.begin("hibernate");
            let (mut peer, fd) = UnixStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            t.retain_lease(OwnedFd::from(fd).into()).unwrap();
            assert_eq!(
                peer.read(&mut [0]).unwrap_err().kind(),
                ErrorKind::WouldBlock
            );
            let guard = t.cancellation_guard();
            if result != "cancelled" {
                t.dispatching();
                t.job_new("/job/1", "systemd-hibernate.service");
                if result == "unknown" {
                    t.lost_monitor("disconnected");
                } else {
                    t.job_removed("/job/1", "systemd-hibernate.service", result);
                }
            }
            drop(guard);
            assert_eq!(peer.read(&mut [0]).unwrap(), 0, "{result}");
        }
    }

    #[test]
    fn cancellation_distinguishes_unsent_from_dispatched_work() {
        let t = Tracker::default();
        t.begin("suspend");
        drop(t.cancellation_guard());
        assert_eq!(t.state.borrow().operation.phase, "failed");
        t.begin("suspend");
        let guard = t.cancellation_guard();
        t.dispatching();
        drop(guard);
        assert_eq!(t.state.borrow().operation.phase, "unknown");
        t.begin("suspend");
        let stale = t.cancellation_guard();
        t.begin("hibernate");
        drop(stale);
        assert_eq!(t.state.borrow().operation.phase, "requested");
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
        assert_eq!(t.state.borrow().operation.phase, "returned");
        t.job_removed("/job/7", "systemd-hibernate.service", "failed");
        assert_eq!(t.state.borrow().operation.phase, "failed");
        assert!(
            t.state
                .borrow()
                .operation
                .error
                .as_ref()
                .unwrap()
                .contains("journal")
        );
        t.accepted();
        assert_eq!(t.state.borrow().operation.phase, "failed");
    }
    #[test]
    fn unknown_untracked_requests_cannot_adopt_later_external_sleep_jobs() {
        let t = Tracker::default();
        t.begin("suspend");
        t.dispatching();
        t.accepted();
        t.lost_monitor("lost subscription");
        t.prepared(true);
        t.job_new("/job/8", "systemd-suspend.service");
        t.job_removed("/job/8", "systemd-suspend.service", "done");
        assert_eq!(t.state.borrow().operation.phase, "unknown");
        assert!(t.state.borrow().operation.job.is_none());
    }

    #[test]
    fn unrelated_or_old_jobs_never_complete_a_new_operation() {
        let t = Tracker::default();
        t.begin("suspend");
        t.dispatching();
        t.job_new("/job/1", "systemd-hibernate.service");
        assert!(t.state.borrow().operation.job.is_none());
        t.job_new("/job/2", "systemd-suspend.service");
        t.begin("suspend");
        t.dispatching();
        t.accepted();
        t.job_removed("/job/2", "systemd-suspend.service", "done");
        assert_eq!(t.state.borrow().operation.phase, "accepted");
        t.job_new("/job/3", "systemd-suspend.service");
        t.lost_monitor("disconnected");
        assert_eq!(t.state.borrow().operation.phase, "unknown");
        t.job_removed("/job/3", "systemd-suspend.service", "done");
        t.failed("lost reply".into(), true);
        assert_eq!(t.state.borrow().operation.phase, "completed");
    }
}
