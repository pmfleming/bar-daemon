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
