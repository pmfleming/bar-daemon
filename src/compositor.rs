//! One event-driven compositor preference cache, shared by every client. Reads
//! use native bounded IPC; failures retain the last known value and retry.
use crate::state::StateStore;
use serde::{Deserialize, Serialize};
use shelllist_hyprland::preferences::Preferences;
use std::{future::Future, time::Duration};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CompositorState {
    pub available: bool,
    pub revision: u64,
    /// None until a successful observation; retained (but unavailable) on error.
    pub animations_enabled: Option<bool>,
    pub error: Option<String>,
}

pub(crate) async fn monitor(store: StateStore) {
    let client = shelllist_hyprland::Client::default();
    monitor_with(store, || client.preferences(), Duration::from_secs(5)).await;
}

async fn monitor_with<F, Fut>(store: StateStore, fetch: F, retry: Duration)
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<Preferences>>,
{
    let mut last_known = None;
    loop {
        let state = match fetch().await {
            Ok(preferences) => {
                last_known = Some(preferences.animations_enabled);
                CompositorState {
                    available: true,
                    animations_enabled: last_known,
                    error: None,
                    ..Default::default()
                }
            }
            Err(error) => CompositorState {
                available: false,
                animations_enabled: last_known,
                error: Some(error.to_string()),
                ..Default::default()
            }
        };
        let failed = !state.available;
        store.update_compositor(state).await;
        // Notify retains one pending invalidation even if it arrives during a
        // read. Healthy preferences have no timer/polling; reconnects and config
        // reloads arrive from the daemon's existing compositor event connection.
        tokio::select! {
            _ = store.compositor_changed.notified() => {},
            _ = tokio::time::sleep(retry), if failed => {},
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
    use tokio::{
        sync::{Semaphore, broadcast},
        time::timeout,
    };

    async fn event(
        events: &mut broadcast::Receiver<crate::state::DomainEvent>,
    ) -> serde_json::Value {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.stream, crate::protocol::stream::COMPOSITOR);
        event.data
    }

    #[tokio::test]
    async fn cache_retries_errors_preserves_last_known_and_deduplicates_events() {
        let store = StateStore::default();
        let mut events = store.subscribe();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(monitor_with(
            store.clone(),
            move || {
                let call = count.fetch_add(1, Ordering::SeqCst);
                async move {
                    match call {
                        0 => anyhow::bail!("offline"),
                        2 => anyhow::bail!("malformed option"),
                        _ => Ok(Preferences {
                            animations_enabled: false,
                        }),
                    }
                }
            },
            Duration::from_millis(30),
        ));
        let unknown = event(&mut events).await;
        assert_eq!(unknown["available"], false);
        assert!(unknown["animations_enabled"].is_null());
        assert_eq!(unknown["revision"], 1);
        assert_eq!(event(&mut events).await["animations_enabled"], false);
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "healthy cache does not poll"
        );
        store.compositor_changed.notify_one();
        let failed = event(&mut events).await;
        assert_eq!(failed["available"], false);
        assert_eq!(
            failed["animations_enabled"], false,
            "retain last known disabled preference"
        );
        assert_eq!(
            event(&mut events).await["available"],
            true,
            "failed reads retry without another event"
        );
        store.compositor_changed.notify_one();
        timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) < 5 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            timeout(Duration::from_millis(50), events.recv())
                .await
                .is_err(),
            "identical observations do not publish"
        );
        let state = store.snapshot().await.compositor;
        assert_eq!(state.animations_enabled, Some(false));
        assert_eq!(state.revision, 4, "only changed observations advance revision");
        task.abort();
    }

    #[tokio::test]
    async fn reload_during_read_is_not_lost_and_reconnect_can_change_preference() {
        let store = StateStore::default();
        let mut events = store.subscribe();
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let finish = Arc::new(Semaphore::new(0));
        let (count, start, gate) = (calls.clone(), started.clone(), finish.clone());
        let task = tokio::spawn(monitor_with(
            store.clone(),
            move || {
                let call = count.fetch_add(1, Ordering::SeqCst);
                let (start, gate) = (start.clone(), gate.clone());
                async move {
                    if call == 0 {
                        start.add_permits(1);
                        gate.acquire().await.unwrap().forget();
                    }
                    Ok(Preferences {
                        animations_enabled: call != 0,
                    })
                }
            },
            Duration::from_secs(5),
        ));
        timeout(Duration::from_secs(2), started.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        store.compositor_changed.notify_one();
        finish.add_permits(1);
        assert_eq!(event(&mut events).await["animations_enabled"], false);
        assert_eq!(event(&mut events).await["animations_enabled"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        task.abort();
    }
}
