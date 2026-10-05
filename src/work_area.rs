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
    let mut dirty = true;
    let mut failed = false;
    let mut next_refresh = Instant::now();
    loop {
        let observed = *demand.borrow_and_update() > 0;
        if observed && (dirty || Instant::now() >= next_refresh) {
            // This read covers earlier invalidations. Those arriving during the
            // read retain a Notify permit and cause a follow-up query.
            let _ = store.work_area_changed.notified().now_or_never();
            let Some(result) = while_observed(fetch(), &mut demand).await else {
                continue;
            };
            let state = match result {
                Ok(monitors) => WorkAreaState {
                    available: true,
                    monitors,
                    ..Default::default()
                },
                Err(error) => WorkAreaState {
                    error: Some(error.to_string()),
                    ..Default::default()
                },
            };
            failed = !state.available;
            next_refresh = Instant::now()
                + if failed || !store.hyprland_connected() {
                    RECOVERY_INTERVAL
                } else {
                    RECONCILE_INTERVAL
                };
            dirty = false;
            store.update_work_area(state).await;
        }
        let recovery = failed || !store.hyprland_connected();
        if !observed {
            // Without events, an idle observation cannot stay fresh. Connected
            // idle caches are invalidated only by geometry/connection changes.
            dirty |= recovery;
            if dirty {
                store.update_work_area(WorkAreaState::default()).await;
            }
        }
        tokio::select! {
            _ = store.work_area_changed.notified() => { dirty = true; },
            _ = tokio::time::sleep_until(next_refresh), if observed => { dirty = true; },
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
