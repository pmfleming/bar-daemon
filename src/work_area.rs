//! One shared work-area cache. Compositor fallback polling exists only while read.
use crate::{model::WorkAreaState, state::StateStore};
use shelllist_hyprland::work_area::Insets;
use std::{collections::BTreeMap, future::Future, time::Duration};
use tokio::sync::watch;

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
    loop {
        if *demand.borrow_and_update() == 0 {
            // Invalidate idle data: the next reader must wait for fresh geometry.
            store.update_work_area(WorkAreaState::default()).await;
            if demand.changed().await.is_err() {
                return;
            }
            continue;
        }
        let state = match fetch().await {
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
        store.update_work_area(state).await;
        tokio::select! {
            _ = store.work_area_changed.notified() => {},
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            changed = demand.changed() => { if changed.is_err() { return; } },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn interest_is_shared_and_drop_stops_polling_and_invalidates_cache() {
        let store = StateStore::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(monitor_with(store.clone(), move || {
            count.fetch_add(1, Ordering::SeqCst);
            async { Ok(BTreeMap::new()) }
        }));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let first = store.work_area_interest();
        let second = store.work_area_interest();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(store.snapshot().await.workarea.available);
        drop(first);
        assert_eq!(*store.work_area_demand().borrow(), 1);
        drop(second);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stopped = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1050)).await;
        assert_eq!(calls.load(Ordering::SeqCst), stopped);
        assert!(!store.snapshot().await.workarea.available);
        task.abort();
    }
}
