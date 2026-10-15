use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use shelllist_daemon_tokio::AbortOnDrop;

use super::{Event, observe, refresh_loop, signal_connection};
use crate::sleep::{
    MANAGER_INTERFACE, MANAGER_PATH,
    lock_tests::{SessionState, fake_logind},
    read_state,
};
use crate::{model::PowerSleepState, state::StateStore};
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    time::{sleep, timeout},
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
