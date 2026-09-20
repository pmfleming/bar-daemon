use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use shelllist_daemon_tokio::AbortOnDrop;

use super::*;
use crate::sleep::{
    MANAGER_INTERFACE,
    lock_tests::{SessionState, fake_logind},
};

async fn wait_for<F>(store: &StateStore, predicate: F)
where
    F: Fn(&PowerSleepState) -> bool,
{
    timeout(Duration::from_secs(2), async {
        loop {
            if predicate(&store.snapshot().await.power_sleep) {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("sleep state was not published promptly");
}

#[tokio::test]
async fn signals_and_clock_recover_while_logind_telemetry_is_stalled() {
    for use_clock in [false, true] {
        let state = Arc::new(SessionState::default());
        state.telemetry_stalled.store(true, Ordering::SeqCst);
        // Force read_state to use the preparation hint from query start. Its
        // late result must not restore preparing=true after a detected resume.
        state.preparing_fails.store(true, Ordering::SeqCst);
        let (server, client) = fake_logind(Arc::clone(&state)).await;
        let store = StateStore::default();
        store.record_sleep_preparation(true).await;
        let (events, mut receiver) = mpsc::channel(32);
        let signal_client = client.clone();
        let signals = tokio::spawn(async move { signal_connection(&signal_client, &events).await });
        let _signals = AbortOnDrop(signals.abort_handle());
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap(),
            Some(Event::Refresh)
        ));
        // The signal subscriptions are established; now start the exact
        // observer and worker used in production, against the isolated bus.
        let (refresh, requests) = watch::channel(());
        let refresh_requests = refresh.clone();
        let offset = Arc::new(AtomicI64::new(0));
        let sample_offset = Arc::clone(&offset);
        let observed_store = store.clone();
        let runner = tokio::spawn(async move {
            tokio::join!(
                observe(
                    &observed_store,
                    receiver,
                    refresh,
                    || Some(i128::from(sample_offset.load(Ordering::SeqCst))),
                    Duration::from_millis(5)
                ),
                refresh_loop(
                    &observed_store,
                    requests,
                    Duration::from_secs(5),
                    |preparing| read_state(&client, preparing)
                ),
            );
        });
        let _runner = AbortOnDrop(runner.abort_handle());
        timeout(Duration::from_secs(2), state.telemetry_started.notified())
            .await
            .unwrap();
        for _ in 0..100 {
            refresh_requests.send_replace(());
        }
        if use_clock {
            // No logind resume signal: the clock fallback must still run.
            offset.store(1_000_000_000, Ordering::SeqCst);
        } else {
            // A short sleep can be reported solely by logind's signal.
            server
                .emit_signal(
                    None::<&str>,
                    MANAGER_PATH,
                    MANAGER_INTERFACE,
                    "PrepareForSleep",
                    &(false,),
                )
                .await
                .unwrap();
        }
        wait_for(&store, |power| {
            power.resume_generation == 1 && !power.preparing_for_sleep
        })
        .await;
        assert_eq!(
            state.telemetry_queries.load(Ordering::SeqCst),
            1,
            "a refresh burst must not spawn parallel queries"
        );
        if use_clock {
            // The later signal describes the same cycle and must not double
            // count it. A following preparation signal is an ordered barrier.
            server
                .emit_signal(
                    None::<&str>,
                    MANAGER_PATH,
                    MANAGER_INTERFACE,
                    "PrepareForSleep",
                    &(false,),
                )
                .await
                .unwrap();
        }
        server
            .emit_signal(
                None::<&str>,
                MANAGER_PATH,
                MANAGER_INTERFACE,
                "PrepareForSleep",
                &(true,),
            )
            .await
            .unwrap();
        wait_for(&store, |power| power.preparing_for_sleep).await;
        assert_eq!(store.snapshot().await.power_sleep.resume_generation, 1);
        // Simulate a second cycle while the *same* original query is stalled.
        offset.store(2_000_000_000, Ordering::SeqCst);
        server
            .emit_signal(
                None::<&str>,
                MANAGER_PATH,
                MANAGER_INTERFACE,
                "PrepareForSleep",
                &(false,),
            )
            .await
            .unwrap();
        wait_for(&store, |power| {
            power.resume_generation == 2 && !power.preparing_for_sleep
        })
        .await;
        state.telemetry_stalled.store(false, Ordering::SeqCst);
        state.telemetry_release.notify_one();
        wait_for(&store, |power| power.available).await;
        let current = store.snapshot().await.power_sleep;
        assert_eq!(current.resume_generation, 2);
        assert!(
            !current.preparing_for_sleep,
            "discard the stale pre-resume reply"
        );
    }
}

#[tokio::test]
async fn clock_detection_does_not_wait_for_signal_connection_setup() {
    let store = StateStore::default();
    // Keep the channel open without a producer: equivalent to a stalled
    // system-bus connection/subscription or reconnect backoff.
    let (_events, receiver) = mpsc::channel(1);
    let (refresh, _requests) = watch::channel(());
    let offset = Arc::new(AtomicI64::new(0));
    let baseline = Arc::new(tokio::sync::Notify::new());
    let sample_offset = Arc::clone(&offset);
    let sampled = Arc::clone(&baseline);
    let observed_store = store.clone();
    let task = tokio::spawn(async move {
        observe(
            &observed_store,
            receiver,
            refresh,
            || {
                let value = sample_offset.load(Ordering::SeqCst);
                sampled.notify_one();
                Some(i128::from(value))
            },
            Duration::from_millis(5),
        )
        .await;
    });
    let _task = AbortOnDrop(task.abort_handle());
    timeout(Duration::from_secs(2), baseline.notified())
        .await
        .unwrap();
    offset.store(1_000_000_000, Ordering::SeqCst);
    wait_for(&store, |power| power.resume_generation == 1).await;
}

struct ActiveQuery(Arc<AtomicBool>);
impl Drop for ActiveQuery {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn telemetry_deadline_and_owner_cancellation_drop_pending_work() {
    let store = StateStore::default();
    let (refresh, requests) = watch::channel(());
    let active = Arc::new(AtomicBool::new(false));
    let started = Arc::new(tokio::sync::Notify::new());
    let query_active = Arc::clone(&active);
    let query_started = Arc::clone(&started);
    let worker_store = store.clone();
    let worker = tokio::spawn(async move {
        refresh_loop(
            &worker_store,
            requests,
            Duration::from_millis(50),
            |_| async {
                query_active.store(true, Ordering::SeqCst);
                let _query = ActiveQuery(Arc::clone(&query_active));
                query_started.notify_one();
                std::future::pending::<Result<PowerSleepState>>().await
            },
        )
        .await;
    });
    let _worker = AbortOnDrop(worker.abort_handle());
    refresh.send_replace(());
    timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert!(active.load(Ordering::SeqCst));
    wait_for(&store, |power| {
        power
            .error
            .as_ref()
            .is_some_and(|error| error.contains("timed out"))
    })
    .await;
    assert!(!store.snapshot().await.power_sleep.available);
    assert!(!active.load(Ordering::SeqCst));
    refresh.send_replace(());
    timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert!(active.load(Ordering::SeqCst));
    worker.abort();
    let _ = worker.await;
    assert!(
        !active.load(Ordering::SeqCst),
        "no detached telemetry work survives cancellation"
    );
    active.store(true, Ordering::SeqCst);
    let query = ActiveQuery(Arc::clone(&active));
    let error = crate::sleep::bounded("preflight", Duration::from_millis(10), async {
        let _query = query;
        std::future::pending::<Result<()>>().await
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("preflight timed out"));
    assert!(
        !active.load(Ordering::SeqCst),
        "bounded preflight must also drop pending work"
    );
}
