//! One event-driven compositor preference cache, shared by every client. Reads
//! use native bounded IPC; failures retain the last known value and retry.
use crate::{model::CompositorState, state::StateStore};
use futures::FutureExt;
use shelllist_hyprland::preferences::Preferences;
use std::{future::Future, time::Duration};

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
        let _ = store.compositor_changed.notified().now_or_never();
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
            },
        };
        let recovery = !state.available || !store.hyprland_connected();
        store.update_compositor(state).await;
        // Notify retains one pending invalidation even if it arrives during a
        // read. Connected, healthy preferences have no polling; recover while
        // event delivery is unavailable even if command reads still succeed.
        tokio::select! {
            _ = store.compositor_changed.notified() => {},
            _ = tokio::time::sleep(retry), if recovery => {},
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
    use tokio::{sync::Semaphore, time::timeout};

    async fn event(events: &mut crate::state::DomainReceivers) -> serde_json::Value {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.stream, crate::protocol::stream::COMPOSITOR);
        event.data
    }

    #[tokio::test]
    async fn reload_during_read_is_not_lost_and_reconnect_can_change_preference() {
        let store = StateStore::default();
        store.set_hyprland_connected(true);
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
        let mut latest = event(&mut events).await;
        if latest["animations_enabled"] == false {
            latest = event(&mut events).await;
        }
        assert_eq!(latest["animations_enabled"], true);
        assert_eq!(
            latest["revision"], 2,
            "both observations committed even when delivery coalesces"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        task.abort();
    }
}
