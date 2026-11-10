//! Compatibility forwarding; request semantics and responses belong to update-daemon.
use serde_json::Value;
pub(super) async fn dispatch(method: &str, params: Value) -> Value {
    match shelllist_update_session::client::call(method, params).await {
        Ok(reply) => reply,
        Err(error) => super::error("update-service-unavailable", error.to_string()),
    }
}
