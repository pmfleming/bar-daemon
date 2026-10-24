//! Direct ingress through the exported in-process service, not a D-Bus proxy.
use super::{owner::Owner, subscriptions::BarDaemon};
use crate::api::OBJECT_PATH;
use anyhow::Result;
use serde_json::Value;
use shelllist_daemon_tokio::{
    BasicCorrelation, JsonlListener, JsonlSocketConfig, LocalJsonlService, SocketSession,
};
use std::sync::Arc;
#[cfg(test)]
mod tests;

struct Service(zbus::Connection);
impl LocalJsonlService for Service {
    async fn call(&self, session: &SocketSession, method: &str, params: Value) -> Result<Value> {
        let interface = self
            .0
            .object_server()
            .interface::<_, BarDaemon>(OBJECT_PATH)
            .await?;
        let daemon = interface.get().await;
        Ok(serde_json::from_str(
            &daemon
                .call_owned(
                    method,
                    &params.to_string(),
                    Some(Owner::Socket(session.clone())),
                )
                .await,
        )?)
    }
    async fn subscribe(&self, session: &SocketSession, streams: Vec<String>) -> Result<Value> {
        let interface = self
            .0
            .object_server()
            .interface::<_, BarDaemon>(OBJECT_PATH)
            .await?;
        let daemon = interface.get().await;
        Ok(serde_json::from_str(
            &daemon
                .subscribe_owned(
                    streams,
                    Some(Owner::Socket(session.clone())),
                    session.clone().into(),
                )
                .await,
        )?)
    }
    async fn cancel(&self, session: &SocketSession, id: &str) -> Result<Value> {
        let interface = self
            .0
            .object_server()
            .interface::<_, BarDaemon>(OBJECT_PATH)
            .await?;
        let daemon = interface.get().await;
        Ok(serde_json::from_str(
            &daemon.cancel_owned(id, Some(session.owner())).await,
        )?)
    }
    async fn disconnected(&self, _: &SocketSession) {
        // Subscription and battery-operation lifetimes observe SocketSession::closed,
        // including tasks admitted after disconnection. Battery cancellation waits
        // for durable reconciliation; dropping its future would claim false rollback.
    }
}
pub(super) async fn listen(
    connection: zbus::Connection,
    stop: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is required for the bar socket"))?;
    JsonlListener::bind(std::path::Path::new(&runtime), "bar-daemon")
        .await?
        .serve(
            Arc::new(Service(connection)),
            BasicCorrelation,
            JsonlSocketConfig::default(),
            stop,
        )
        .await
}
