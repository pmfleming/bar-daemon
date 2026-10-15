use super::monitor_with;
use crate::state::StateStore;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{task::yield_now, time::advance};

async fn tick(seconds: u64) {
    advance(Duration::from_secs(seconds)).await;
    yield_now().await;
}

#[tokio::test(start_paused = true)]
async fn failed_queries_retry_without_events_then_stop_fast_polling() {
    let store = StateStore::default();
    store.set_hyprland_connected(true);
    let _reader = store.work_area_interest();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let task = tokio::spawn(monitor_with(store.clone(), move || {
        let call = count.fetch_add(1, Ordering::SeqCst);
        async move {
            if call == 0 {
                anyhow::bail!("offline");
            }
            Ok(BTreeMap::new())
        }
    }));
    yield_now().await;
    assert_eq!(
        store.snapshot().await.workarea.error.as_deref(),
        Some("offline")
    );
    tick(1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(store.snapshot().await.workarea.available);
    tick(29).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = task.await;
}
