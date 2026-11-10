//! Temporary in-process transport adapter for the update-owned session domain.
use crate::{model::UpdateState, state::StateStore};
use anyhow::Result;
use std::sync::Arc;
pub(crate) use shelllist_update_session::updates::{history, open};

pub(crate) async fn refresh_default(store: &StateStore) -> Result<UpdateState> {
    let updates = shelllist_update_session::state::StateStore::default();
    let value = shelllist_update_session::updates::refresh_default(&updates).await?;
    store.update_updates(value.clone()).await;
    Ok(value)
}

pub(crate) async fn monitor(
    store: StateStore,
    service: Arc<crate::activity::notifications::service::NotificationService>,
) {
    let updates = shelllist_update_session::state::StateStore::default();
    let mut events = updates.subscribe();
    let sink = shelllist_update_session::notification::NotificationService::new(
        service.durable_updates(),
        move |key, run_id, incoming| {
            let service = service.clone();
            Box::pin(async move {
                let incoming = serde_json::from_value(serde_json::to_value(incoming)?)?;
                service.notify_update_once(key, run_id, incoming).await
            })
        },
    );
    let forward = async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Ok(value) = serde_json::from_value(event.data) {
                        store.update_updates(value).await;
                        store.invalidate_update_history().await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    store.update_updates(updates.snapshot().await).await;
                    store.invalidate_update_history().await;
                }
                Err(_) => break,
            }
        }
    };
    tokio::select! {
        _ = shelllist_update_session::updates::monitor(updates.clone(), sink) => {},
        _ = forward => {},
    }
}
