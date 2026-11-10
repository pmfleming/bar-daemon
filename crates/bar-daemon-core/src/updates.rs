//! Compatibility transport to the independent update owner. No storage access.
use crate::{model::UpdateState, state::StateStore};
use anyhow::Result;
pub(crate) use shelllist_update_session::client::open;
use std::sync::Arc;

pub(crate) async fn refresh_default(store: &StateStore) -> Result<UpdateState> {
    let value = shelllist_update_session::client::refresh().await?;
    store.update_updates(value.clone()).await;
    Ok(value)
}

pub(crate) async fn monitor(
    store: StateStore,
    _service: Arc<crate::activity::notifications::service::NotificationService>,
) {
    loop {
        let result = shelllist_update_session::client::watch(|value| {
            let store = store.clone();
            async move {
                store.update_updates(value).await;
                store.invalidate_update_history().await;
            }
        })
        .await;
        store
            .update_updates(UpdateState {
                error: Some(
                    result
                        .err()
                        .map_or_else(|| "Update service disconnected".into(), |e| e.to_string()),
                ),
                ..Default::default()
            })
            .await;
        store.invalidate_update_history().await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
