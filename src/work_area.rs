//! Demand-owned geometry cache: events plus slow reconciliation while connected,
//! fast recovery when disconnected. Idle caches survive until invalidated.
use crate::{model::WorkAreaState, state::StateStore};
use futures::FutureExt;
use shelllist_hyprland::work_area::Insets;
use std::{collections::BTreeMap, future::Future, time::Duration};
use tokio::{sync::watch, time::Instant};

const RECOVERY_INTERVAL: Duration = Duration::from_secs(1);
// Layer-surface reservation changes need not emit a socket event.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) struct Interest(watch::Sender<usize>);
impl Interest {
    pub fn new(sender: watch::Sender<usize>) -> Self {
        sender.send_modify(|count| *count += 1);
        Self(sender)
    }
}
impl Drop for Interest {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count = count.saturating_sub(1));
    }
}
#[derive(Debug, PartialEq, Eq)]
enum CacheAction {
    Fetch,
    Clear,
    Wait,
}

struct RefreshPolicy {
    dirty: bool,
    failed: bool,
    next_refresh: Instant,
}

impl RefreshPolicy {
    fn new(now: Instant) -> Self {
        Self {
            dirty: true,
            failed: false,
            next_refresh: now,
        }
    }

    fn action(&mut self, observed: bool, connected: bool, now: Instant) -> CacheAction {
        if observed {
            return if self.dirty || now >= self.next_refresh {
                CacheAction::Fetch
            } else {
                CacheAction::Wait
            };
        }
        // Disconnected/failed idle observations cannot stay fresh without events.
        self.dirty |= self.failed || !connected;
        if self.dirty {
            CacheAction::Clear
        } else {
            CacheAction::Wait
        }
    }

    fn completed(&mut self, available: bool, connected: bool, now: Instant) {
        self.failed = !available;
        self.dirty = false;
        self.next_refresh = now
            + if self.failed || !connected {
                RECOVERY_INTERVAL
            } else {
                RECONCILE_INTERVAL
            };
    }
}

fn observed_state(result: anyhow::Result<BTreeMap<String, Insets>>) -> WorkAreaState {
    match result {
        Ok(monitors) => WorkAreaState {
            available: true,
            monitors,
            ..Default::default()
        },
        Err(error) => WorkAreaState {
            error: Some(error.to_string()),
            ..Default::default()
        },
    }
}

pub(crate) async fn monitor(store: StateStore) {
    let client = shelllist_hyprland::Client::default();
    monitor_with(store, || client.work_areas()).await;
}
async fn monitor_with<F, Fut>(store: StateStore, fetch: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<BTreeMap<String, Insets>>>,
{
    let mut demand = store.work_area_demand();
    let mut policy = RefreshPolicy::new(Instant::now());
    loop {
        let observed = *demand.borrow_and_update() > 0;
        match policy.action(observed, store.hyprland_connected(), Instant::now()) {
            CacheAction::Fetch => {
                // Consume only earlier invalidations. A notification arriving
                // during the read retains its permit for a follow-up query.
                let _ = store.work_area_changed.notified().now_or_never();
                let Some(result) = while_observed(fetch(), &mut demand).await else {
                    continue;
                };
                let state = observed_state(result);
                policy.completed(state.available, store.hyprland_connected(), Instant::now());
                store.update_work_area(state).await;
            }
            CacheAction::Clear => store.update_work_area(WorkAreaState::default()).await,
            CacheAction::Wait => {}
        }
        tokio::select! {
            _ = store.work_area_changed.notified() => { policy.dirty = true; },
            _ = tokio::time::sleep_until(policy.next_refresh), if observed => { policy.dirty = true; },
            changed = demand.changed() => { if changed.is_err() { return; } },
        }
    }
}

async fn while_observed<T>(
    future: impl Future<Output = T>,
    demand: &mut watch::Receiver<usize>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            changed = demand.changed() => {
                if changed.is_err() || *demand.borrow_and_update() == 0 { return None; }
            }
            result = &mut future => return Some(result),
        }
    }
}

#[cfg(test)]
mod tests;
