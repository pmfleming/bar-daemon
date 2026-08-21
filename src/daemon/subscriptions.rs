use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Result;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use zbus::{message::Header, object_server::SignalEmitter};

use crate::{
    api::{self, ApiService},
    model::BarSnapshot,
    protocol,
    state::StateStore,
};

struct Subscription {
    owner: Option<String>,
    task: JoinHandle<()>,
}

type Subscriptions = Arc<Mutex<HashMap<String, Subscription>>>;

pub(super) struct BarDaemon {
    api: ApiService,
    state: StateStore,
    sequence: AtomicU64,
    subscriptions: Subscriptions,
}

impl BarDaemon {
    pub(super) fn new(api: ApiService, state: StateStore) -> Self {
        Self {
            api,
            state,
            sequence: AtomicU64::new(1),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
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
        let id = format!(
            "subscription-{}",
            self.sequence.fetch_add(1, Ordering::Relaxed)
        );
        let owner = header.sender().map(ToString::to_string);
        let connection = emitter.connection().clone();
        let directed = directed_emitter(&emitter, &header);
        let state = self.state.clone();
        let (start_sender, start_receiver) = oneshot::channel();
        let task_id = id.clone();
        let task_owner = owner.clone();
        let subscriptions = Arc::clone(&self.subscriptions);
        let task = tokio::spawn(async move {
            if start_receiver.await.is_err() {
                return;
            }
            let events = forward_events(state, directed, task_id.clone(), streams);
            if let Some(owner) = task_owner {
                tokio::select! {
                    _ = events => {}
                    _ = wait_for_owner_loss(connection, owner) => {}
                }
            } else {
                events.await;
            }
            subscriptions.lock().await.remove(&task_id);
        });
        self.subscriptions
            .lock()
            .await
            .insert(id.clone(), Subscription { owner, task });
        let _ = start_sender.send(());
        api::success(json!({ "subscription": { "id": id } })).to_string()
    }

    async fn cancel(&self, request_id: &str, #[zbus(header)] header: Header<'_>) -> String {
        let owner = header.sender().map(ToString::to_string);
        let mut subscriptions = self.subscriptions.lock().await;
        let owned = subscriptions
            .get(request_id)
            .is_some_and(|subscription| subscription.owner == owner);
        if owned && let Some(subscription) = subscriptions.remove(request_id) {
            subscription.task.abort();
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

fn directed_emitter(emitter: &SignalEmitter<'_>, header: &Header<'_>) -> SignalEmitter<'static> {
    match header.sender() {
        Some(sender) => emitter.to_owned().set_destination(sender.to_owned().into()),
        None => emitter.to_owned(),
    }
}

async fn forward_events(
    state: StateStore,
    emitter: SignalEmitter<'static>,
    subscription_id: String,
    streams: Vec<String>,
) {
    let (snapshot, mut events) = state.snapshot_and_subscribe().await;
    for stream in &streams {
        let data = initial_stream_data(stream, &snapshot);
        emit_event(&emitter, stream, "subscribed", &subscription_id, data).await;
    }
    loop {
        match events.recv().await {
            Ok(event) if stream_selected(&streams, &event.stream) => {
                emit_event(
                    &emitter,
                    &event.stream,
                    "changed",
                    &subscription_id,
                    event.data,
                )
                .await;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                for stream in &streams {
                    emit_event(
                        &emitter,
                        stream,
                        "lagged",
                        &subscription_id,
                        json!({ "missed": count }),
                    )
                    .await;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn stream_selected(streams: &[String], event_stream: &str) -> bool {
    streams.iter().any(|stream| stream == event_stream)
}

fn initial_stream_data(stream: &str, snapshot: &BarSnapshot) -> Value {
    match stream {
        protocol::stream::ACTIVITY => {
            serde_json::to_value(&snapshot.activity).unwrap_or(Value::Null)
        }
        protocol::stream::WORKSPACES => {
            serde_json::to_value(&snapshot.workspaces).unwrap_or(Value::Null)
        }
        protocol::stream::MEDIA => serde_json::to_value(&snapshot.media).unwrap_or(Value::Null),
        protocol::stream::AUDIO => serde_json::to_value(&snapshot.audio).unwrap_or(Value::Null),
        protocol::stream::BRIGHTNESS => {
            serde_json::to_value(&snapshot.brightness).unwrap_or(Value::Null)
        }
        protocol::stream::BATTERY => serde_json::to_value(&snapshot.battery).unwrap_or(Value::Null),
        protocol::stream::POWER_PROFILE => {
            serde_json::to_value(&snapshot.power_profile).unwrap_or(Value::Null)
        }
        protocol::stream::POWER_SLEEP => {
            serde_json::to_value(&snapshot.power_sleep).unwrap_or(Value::Null)
        }
        protocol::stream::NOTIFICATIONS => {
            serde_json::to_value(&snapshot.notifications).unwrap_or(Value::Null)
        }
        protocol::stream::NOTIFICATION_ACTIVE => {
            serde_json::to_value(&snapshot.notification_active).unwrap_or(Value::Null)
        }
        protocol::stream::UPDATES => serde_json::to_value(&snapshot.updates).unwrap_or(Value::Null),
        protocol::stream::TIMEZONE => {
            serde_json::to_value(&snapshot.timezone).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

async fn emit_event(
    emitter: &SignalEmitter<'_>,
    stream: &str,
    event: &str,
    subscription_id: &str,
    data: Value,
) {
    let value = json!({
        "protocol": protocol::NAME,
        "version": protocol::VERSION,
        "stream": stream,
        "event": event,
        "subscription_id": subscription_id,
        "data": data
    });
    if let Err(error) = BarDaemon::event(emitter, stream, &value.to_string()).await {
        tracing::warn!(%stream, %error, "bar-api event could not be emitted");
    }
}

async fn wait_for_owner_loss(connection: zbus::Connection, owner: String) {
    let result: Result<()> = async {
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?;
        let mut changes = proxy.receive_signal("NameOwnerChanged").await?;
        let has_owner: bool = proxy.call("NameHasOwner", &(owner.as_str(),)).await?;
        tracing::debug!(%owner, has_owner, "subscription owner watch established");
        if !has_owner {
            return Ok(());
        }
        while let Some(message) = changes.next().await {
            let (name, old_owner, new_owner): (String, String, String) =
                message.body().deserialize()?;
            if name == owner && !old_owner.is_empty() && new_owner.is_empty() {
                return Ok(());
            }
        }
        anyhow::bail!("D-Bus owner-change stream ended")
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%owner, %error, "subscription owner watcher stopped");
    }
}

#[cfg(test)]
mod tests {
    use crate::{model::BarSnapshot, protocol};

    use super::{initial_stream_data, stream_selected};

    #[test]
    fn selects_only_subscribed_event_streams() {
        let streams = vec![
            protocol::stream::AUDIO.into(),
            protocol::stream::MEDIA.into(),
        ];
        assert!(stream_selected(&streams, protocol::stream::AUDIO));
        assert!(!stream_selected(&streams, protocol::stream::BATTERY));
    }

    #[test]
    fn initial_subscription_includes_current_domain_state() {
        let data = initial_stream_data(protocol::stream::WORKSPACES, &BarSnapshot::default());
        assert_eq!(data["available"], false);
    }
}
