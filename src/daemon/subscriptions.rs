use std::sync::Arc;

use serde_json::{Value, json};
use shelllist_daemon_tokio::{
    BroadcastEvent, OwnedTaskRegistry, directed_emitter, forward_broadcast,
};
use zbus::{message::Header, object_server::SignalEmitter};

use crate::{
    api::{self, ApiService},
    protocol::{self, read_wire::Event},
    state::StateStore,
};

pub(super) struct BarDaemon {
    api: ApiService,
    state: StateStore,
    pub(super) subscriptions: Arc<OwnedTaskRegistry>,
}

impl BarDaemon {
    pub(super) fn new(api: ApiService, state: StateStore) -> Self {
        Self {
            api,
            state,
            subscriptions: Arc::new(OwnedTaskRegistry::default()),
        }
    }
}

#[zbus::interface(name = "org.laufan.BarDaemon1")]
impl BarDaemon {
    async fn call(&self, method: &str, params_json: &str) -> String {
        let params: Value = match serde_json::from_str(params_json) {
            Ok(value) => value,
            Err(error) => {
                return api::error("validation-error", format!("invalid params JSON: {error}"))
                    .to_string();
            }
        };
        self.api.dispatch(method, params).await.to_string()
    }

    async fn subscribe(
        &self,
        streams: Vec<String>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> String {
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
    let (snapshot, events) = state.snapshot_and_subscribe().await;
    for stream in &streams {
        emit_event(
            &emitter,
            Event::subscribed(stream, &subscription_id, &snapshot),
        )
        .await;
    }
    forward_broadcast(events, |update| {
        let (emitter, subscription_id, streams) = (&emitter, &subscription_id, &streams);
        async move {
            match update {
                BroadcastEvent::Item(event)
                    if streams.iter().any(|stream| stream == &event.stream) =>
                {
                    emit_event(emitter, Event::changed(subscription_id, event)).await;
                }
                BroadcastEvent::Item(_) => {}
                BroadcastEvent::Lagged(count) => {
                    for stream in streams {
                        emit_event(emitter, Event::lagged(stream, subscription_id, count)).await;
                    }
                }
            }
        }
    })
    .await;
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
