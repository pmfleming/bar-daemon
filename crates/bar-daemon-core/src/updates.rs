//! Compatibility transport to the independent update owner. No storage access.
use crate::{model::UpdateState, state::StateStore};
use anyhow::Result;
pub(crate) use shelllist_update_session::client::open;

pub(crate) async fn refresh_default(store: &StateStore) -> Result<UpdateState> {
    let value = shelllist_update_session::client::refresh().await?;
    store.update_updates(value.clone()).await;
    Ok(value)
}

pub(crate) async fn monitor(store: StateStore) {
    let publish = |value| {
        let store = store.clone();
        async move {
            store.update_updates(value).await;
            store.invalidate_update_history().await;
        }
    };
    loop {
        let result = shelllist_update_session::client::watch(&publish).await;
        publish(UpdateState {
            error: Some(
                result
                    .err()
                    .map_or_else(|| "Update service disconnected".into(), |e| e.to_string()),
            ),
            ..Default::default()
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
