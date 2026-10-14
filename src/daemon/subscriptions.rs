use std::sync::Arc;

use serde_json::{Value, json};
use shelllist_daemon_tokio::{OwnedTaskRegistry, directed_emitter};
use tokio::sync::broadcast::error::RecvError;
use zbus::{message::Header, object_server::SignalEmitter};

use crate::{
    api::{self, ApiService},
    protocol::{self, read_wire::Event},
    state::StateStore,
};

pub(super) struct BarDaemon {
    boundary: shelllist_daemon_tokio::ServiceBoundary,
    api: Arc<ApiService>,
    operations: super::battery_operations::Operations,
    state: StateStore,
    pub(super) subscriptions: Arc<OwnedTaskRegistry>,
}

impl BarDaemon {
    pub(super) fn new(api: ApiService, state: StateStore) -> Self {
        Self {
            boundary: shelllist_daemon_tokio::ServiceBoundary::new(
                shelllist_daemon_core::ApiIdentity::new(protocol::NAME, protocol::VERSION as u32),
            ),
            api: Arc::new(api),
            operations: super::battery_operations::Operations::default(),
            state,
            subscriptions: Arc::new(OwnedTaskRegistry::default()),
        }
    }
}

#[zbus::interface(name = "org.laufan.BarDaemon2")]
impl BarDaemon {
    async fn call(
        &self,
        method: &str,
        params_json: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> String {
        let policy = protocol::method_policy(method);
        let owner = header.sender().map(|s| s.as_str());
        let admission = match policy {
            Some(policy) => self
                .boundary
                .call_policy(owner, method, params_json, policy),
            None => self.boundary.call(owner, method, params_json),
        };
        let admitted = match admission {
            Ok(admitted) => admitted,
            Err(response) => return response,
        };
        let params: Value = match serde_json::from_str(params_json) {
            Ok(value) => value,
            Err(error) => {
                return api::error("validation-error", format!("invalid params JSON: {error}"))
                    .to_string();
            }
        };
        if matches!(
            method,
            "battery.operation.start" | "battery.operation.status" | "battery.operation.cancel"
        ) {
            let Some(owner) = header.sender() else {
                return api::error("owner-required", "operation requires a bus owner").to_string();
            };
            return admitted.encode(&self.operations.call(
                method,
                params,
                owner.as_str(),
                connection,
                self.api.clone(),
            ));
        }
        if policy.is_some() {
            let api = self.api.clone();
            let method = method.to_owned();
            admitted
                .run(async move { api.dispatch(&method, params).await })
                .await
        } else {
            admitted.encode(&self.api.dispatch(method, params).await)
        }
    }

    async fn subscribe(
        &self,
        streams: Vec<String>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> String {
        let _admitted = match self
            .boundary
            .subscribe(header.sender().map(|s| s.as_str()), &streams)
        {
            Ok(admitted) => admitted,
            Err(response) => return response,
        };
        if let Some(stream) = streams
            .iter()
            .find(|stream| !protocol::STREAMS.contains(&stream.as_str()))
        {
            return api::error(
                "unsupported-stream",
                format!("Unsupported bar-api stream: {stream}"),
            )
            .to_string();
        }
        let id = self.subscriptions.next_id("subscription");
        let owner = header.sender().map(ToString::to_string);
        let connection = emitter.connection().clone();
        let directed = directed_emitter(&emitter, &header);
        let state = self.state.clone();
        if let Err(error) = self.subscriptions.spawn_for_owner(
            id.clone(),
            owner,
            &connection,
            forward_events(state, directed, id.clone(), streams),
        ) {
            return api::error("subscription-unavailable", error.to_string()).to_string();
        }
        api::success(json!({ "subscription": { "id": id } })).to_string()
    }

    async fn cancel(&self, request_id: &str, #[zbus(header)] header: Header<'_>) -> String {
        let _admitted = match self
            .boundary
            .cancel(header.sender().map(|s| s.as_str()), request_id)
        {
            Ok(admitted) => admitted,
            Err(response) => return response,
        };
        let owner = header.sender().map(ToString::to_string);
        if self
            .subscriptions
            .cancel_owned(request_id, owner.as_deref())
            .await
        {
            return api::success(json!({ "cancelled": request_id, "kind": "subscription" }))
                .to_string();
        }
        api::error(
            "request-not-found",
            format!("No active request named {request_id}"),
        )
        .to_string()
    }

    #[zbus(signal)]
    async fn event(emitter: &SignalEmitter<'_>, stream: &str, event_json: &str)
    -> zbus::Result<()>;
}

async fn forward_events(
    state: StateStore,
    emitter: SignalEmitter<'static>,
    subscription_id: String,
    streams: Vec<String>,
) {
    let _work_area_interest = streams
        .iter()
        .any(|stream| stream == protocol::stream::WORKAREA)
        .then(|| state.work_area_interest());
    let streams: Vec<_> = streams
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let names: Vec<_> = streams.iter().map(String::as_str).collect();
    let (snapshot, events) = state.snapshot_and_subscribe_domains(&names).await;
    // JoinSet aborts all domain workers when owner cancellation drops this
    // future. No extra shared queue can turn one domain's lag into global loss.
    let mut workers = tokio::task::JoinSet::new();
    for (stream, mut events) in streams.into_iter().zip(events.0) {
        let initial = Event::subscribed(&stream, &subscription_id, &snapshot);
        let emitter = emitter.clone();
        let subscription_id = subscription_id.clone();
        workers.spawn(async move {
            emit_event(&emitter, initial).await;
            loop {
                match events.recv().await {
                    Ok(event) => {
                        emit_event(&emitter, Event::changed(&subscription_id, event)).await
                    }
                    Err(RecvError::Lagged(count)) => {
                        tracing::warn!(%stream, dropped_events = count, "bar subscriber lag");
                        emit_event(&emitter, Event::lagged(&stream, &subscription_id, count)).await;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
    while let Some(result) = workers.join_next().await {
        if let Err(error) = result {
            tracing::error!(%error, "bar domain forwarder stopped");
        }
    }
}

async fn emit_event(emitter: &SignalEmitter<'_>, event: Event) {
    let stream = event.stream();
    if let Err(error) = emitter
        .emit(
            api::INTERFACE,
            "Event",
            &(stream, event.value().to_string()),
        )
        .await
    {
        tracing::warn!(%stream, %error, "bar-api event could not be emitted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::time::{Duration, timeout};

    struct Peer;
    #[zbus::interface(name = "org.laufan.BarDaemon2")]
    impl Peer {
        fn ping(&self) {}
    }

    #[tokio::test]
    async fn real_signals_keep_initial_handoff_and_burst_isolation() {
        let state = StateStore::default();
        let (server, client) = timeout(
            Duration::from_secs(2),
            crate::test_support::dbus_peer(|builder| builder.serve_at(api::OBJECT_PATH, Peer)),
        )
        .await
        .unwrap()
        .unwrap();
        let proxy = zbus::Proxy::new(&client, api::BUS_NAME, api::OBJECT_PATH, api::INTERFACE)
            .await
            .unwrap();
        let mut signals = proxy.receive_signal("Event").await.unwrap();
        let emitter = SignalEmitter::new(&server, api::OBJECT_PATH)
            .unwrap()
            .to_owned();
        let forwarder = tokio::spawn(forward_events(
            state.clone(),
            emitter,
            "test-sub".into(),
            vec![
                protocol::stream::BATTERY.into(),
                protocol::stream::NOTIFICATION_ACTIVE.into(),
            ],
        ));
        for _ in 0..2 {
            let message = timeout(Duration::from_secs(2), signals.next())
                .await
                .unwrap()
                .unwrap();
            let (stream, encoded): (String, String) = message.body().deserialize().unwrap();
            let value: Value = serde_json::from_str(&encoded).unwrap();
            assert_eq!(value["event"], "subscribed");
            assert_eq!(value["stream"], stream);
            assert_eq!(value["subscription_id"], "test-sub");
        }
        for n in 0..2048 {
            state
                .update_audio(crate::model::AudioState {
                    available: true,
                    volume_percent: (n % 100) as u8,
                    ..Default::default()
                })
                .await;
        }
        state
            .update_battery(crate::model::BatteryState {
                available: true,
                percentage: 42,
                ..Default::default()
            })
            .await;
        let message = timeout(Duration::from_secs(2), signals.next())
            .await
            .unwrap()
            .unwrap();
        let (stream, encoded): (String, String) = message.body().deserialize().unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(stream, protocol::stream::BATTERY);
        assert_eq!(
            value["event"], "changed",
            "audio burst cannot send gaps to battery/notification consumers"
        );
        assert_eq!(value["data"]["percentage"], 42);
        assert!(
            timeout(Duration::from_millis(20), signals.next())
                .await
                .is_err()
        );
        forwarder.abort();
        assert!(forwarder.await.unwrap_err().is_cancelled());

        let emitter = SignalEmitter::new(&server, api::OBJECT_PATH)
            .unwrap()
            .to_owned();
        let forwarder = tokio::spawn(forward_events(
            state.clone(),
            emitter,
            "geometry".into(),
            vec![protocol::stream::WORKAREA.into()],
        ));
        timeout(Duration::from_secs(2), signals.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*state.work_area_demand().borrow(), 1);
        forwarder.abort();
        assert!(forwarder.await.unwrap_err().is_cancelled());
        assert_eq!(
            *state.work_area_demand().borrow(),
            0,
            "owner cancellation releases demand and domain workers"
        );
    }
}
