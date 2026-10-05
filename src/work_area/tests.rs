use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{sync::Semaphore, task::yield_now, time::advance};

fn counting_monitor(store: &StateStore) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let task = tokio::spawn(monitor_with(store.clone(), move || {
        count.fetch_add(1, Ordering::SeqCst);
        async { Ok(BTreeMap::new()) }
    }));
    (calls, task)
}

async fn tick(seconds: u64) {
    advance(Duration::from_secs(seconds)).await;
    yield_now().await;
}

#[tokio::test(start_paused = true)]
async fn healthy_cache_is_shared_event_driven_and_retained_until_invalidated() {
    let store = StateStore::default();
    store.set_hyprland_connected(true);
    let (calls, task) = counting_monitor(&store);
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let first = store.work_area_interest();
    let second = store.work_area_interest();
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let revision = store.snapshot().await.workarea.revision;
    tick(10).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "healthy subscriptions do not poll every second"
    );
    drop(first);
    yield_now().await;
    assert_eq!(*store.work_area_demand().borrow(), 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "removing one reader is not an invalidation"
    );
    drop(second);
    yield_now().await;
    tick(10).await;
    assert!(store.snapshot().await.workarea.available);
    let reader = store.work_area_interest();
    yield_now().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "fresh idle cache is reusable"
    );
    store.work_area_changed.notify_one();
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.snapshot().await.workarea.revision,
        revision,
        "identical reads do not publish"
    );
    drop(reader);
    yield_now().await;
    store.work_area_changed.notify_one();
    yield_now().await;
    assert!(
        !store.snapshot().await.workarea.available,
        "idle invalidations prevent stale reuse"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "idle invalidations do not query"
    );
    let _reader = store.work_area_interest();
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(store.snapshot().await.workarea.available);
    task.abort();
    let _ = task.await;
}

#[tokio::test(start_paused = true)]
async fn disconnected_polling_requires_demand_and_stops_when_events_recover() {
    let store = StateStore::default();
    let (calls, task) = counting_monitor(&store);
    yield_now().await;
    tick(30).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let reader = store.work_area_interest();
    yield_now().await;
    tick(1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(reader);
    yield_now().await;
    assert!(!store.snapshot().await.workarea.available);
    tick(30).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    store.set_hyprland_connected(true);
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let _reader = store.work_area_interest();
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    tick(29).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    store.set_hyprland_connected(false);
    yield_now().await;
    tick(1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    store.set_hyprland_connected(true);
    yield_now().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "reconnect reconciles immediately"
    );
    tick(29).await;
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    task.abort();
    let _ = task.await;
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

#[tokio::test(start_paused = true)]
async fn invalidation_during_a_read_triggers_one_followup() {
    let store = StateStore::default();
    store.set_hyprland_connected(true);
    let _reader = store.work_area_interest();
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let (count, blocked) = (calls.clone(), gate.clone());
    let task = tokio::spawn(monitor_with(store.clone(), move || {
        let call = count.fetch_add(1, Ordering::SeqCst);
        let blocked = blocked.clone();
        async move {
            if call == 0 {
                blocked.acquire().await.unwrap().forget();
            }
            Ok(BTreeMap::new())
        }
    }));
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for _ in 0..100 {
        store.work_area_changed.notify_one();
    }
    gate.add_permits(1);
    yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    tick(29).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = task.await;
}

#[tokio::test(start_paused = true)]
async fn silent_changes_reconcile_slowly_and_old_idle_cache_refreshes_on_demand() {
    let store = StateStore::default();
    store.set_hyprland_connected(true);
    let reader = store.work_area_interest();
    let (calls, task) = counting_monitor(&store);
    yield_now().await;
    tick(29).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    tick(1).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "reconcile geometry changes lacking events"
    );
    drop(reader);
    yield_now().await;
    tick(60).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "no idle reconciliation queries"
    );
    let _reader = store.work_area_interest();
    yield_now().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "expired idle data is revalidated"
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test(start_paused = true)]
async fn dropping_last_reader_cancels_inflight_query() {
    struct ReadGuard(Arc<AtomicBool>);
    impl Drop for ReadGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let store = StateStore::default();
    let reader = store.work_area_interest();
    let cancelled = Arc::new(AtomicBool::new(false));
    let marker = cancelled.clone();
    let task = tokio::spawn(monitor_with(store.clone(), move || {
        let guard = ReadGuard(marker.clone());
        async move {
            let _guard = guard;
            std::future::pending::<anyhow::Result<BTreeMap<String, Insets>>>().await
        }
    }));
    yield_now().await;
    assert!(!cancelled.load(Ordering::SeqCst));
    drop(reader);
    yield_now().await;
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(!store.snapshot().await.workarea.available);
    tick(30).await;
    task.abort();
    let _ = task.await;
}
