use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::{
    signal::{
        ctrl_c,
        unix::{SignalKind, signal},
    },
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use zbus::{connection, message::Header, object_server::SignalEmitter};

use crate::{
    api::{self, ApiService, BUS_NAME, OBJECT_PATH},
    audio, battery, brightness, hyprland, media, notifications, power, protocol,
    state::StateStore,
    timezone, updates,
};

struct Subscription {
    owner: Option<String>,
    task: JoinHandle<()>,
}

type Subscriptions = Arc<Mutex<HashMap<String, Subscription>>>;

pub struct BarDaemon {
    api: ApiService,
    state: StateStore,
    sequence: AtomicU64,
    subscriptions: Subscriptions,
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
            Ok(event) if streams.contains(&event.stream) => {
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

fn initial_stream_data(stream: &str, snapshot: &crate::model::BarSnapshot) -> Value {
    match stream {
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
        protocol::stream::NOTIFICATIONS => {
            serde_json::to_value(&snapshot.notifications).unwrap_or(Value::Null)
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
        "protocol": api::PROTOCOL,
        "version": api::VERSION,
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

pub async fn run() -> Result<()> {
    let state = StateStore::default();
    let api = ApiService::new(state.clone());
    let subscriptions = Arc::new(Mutex::new(HashMap::new()));
    let daemon = BarDaemon {
        api,
        state: state.clone(),
        sequence: AtomicU64::new(1),
        subscriptions: Arc::clone(&subscriptions),
    };
    let _connection = connection::Builder::session()
        .context("connect to session D-Bus")?
        .name(BUS_NAME)
        .context("claim bar-daemon bus name")?
        .serve_at(OBJECT_PATH, daemon)
        .context("export bar-daemon interface")?
        .build()
        .await
        .context("start bar-daemon D-Bus service")?;

    let workspace_task = tokio::spawn(hyprland::monitor(state.clone()));
    let media_task = tokio::spawn(media::monitor(state.clone()));
    let audio_task = tokio::spawn(audio::monitor(state.clone()));
    let brightness_task = tokio::spawn(brightness::monitor(state.clone()));
    let battery_task = tokio::spawn(battery::monitor(state.clone()));
    let power_task = tokio::spawn(power::monitor(state.clone()));
    let notification_task = tokio::spawn(notifications::monitor(state.clone()));
    let update_task = tokio::spawn(updates::monitor(state.clone()));
    let timezone_task = tokio::spawn(timezone::monitor(state));
    tracing::info!(
        bus_name = BUS_NAME,
        object_path = OBJECT_PATH,
        "bar-daemon started"
    );
    let mut terminate = signal(SignalKind::terminate()).context("listen for SIGTERM")?;
    let result = tokio::select! {
        result = ctrl_c() => result.context("wait for Ctrl-C"),
        _ = terminate.recv() => Ok(()),
    };
    workspace_task.abort();
    media_task.abort();
    audio_task.abort();
    brightness_task.abort();
    battery_task.abort();
    power_task.abort();
    notification_task.abort();
    update_task.abort();
    timezone_task.abort();
    result
}

#[cfg(test)]
mod tests {
    use crate::{model::BarSnapshot, protocol};

    use super::initial_stream_data;

    #[test]
    fn initial_subscription_includes_current_domain_state() {
        let data = initial_stream_data(protocol::stream::WORKSPACES, &BarSnapshot::default());
        assert_eq!(data["available"], false);
    }
}
