use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::{
    signal::{
        ctrl_c,
        unix::{SignalKind, signal},
    },
    sync::Mutex,
    task::JoinHandle,
};
use zbus::{connection, object_server::SignalEmitter};

use crate::{
    api::{self, ApiService, BUS_NAME, OBJECT_PATH},
    hyprland, media, protocol,
    state::StateStore,
};

pub struct BarDaemon {
    api: ApiService,
    state: StateStore,
    sequence: AtomicU64,
    subscriptions: Mutex<HashMap<String, JoinHandle<()>>>,
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
        let task = tokio::spawn(forward_events(
            self.state.clone(),
            emitter.to_owned(),
            id.clone(),
            streams,
        ));
        self.subscriptions.lock().await.insert(id.clone(), task);
        api::success(json!({ "subscription": { "id": id } })).to_string()
    }

    async fn cancel(&self, request_id: &str) -> String {
        if let Some(task) = self.subscriptions.lock().await.remove(request_id) {
            task.abort();
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
    let snapshot = state.snapshot().await;
    for stream in &streams {
        let data = initial_stream_data(stream, &snapshot);
        emit_event(&emitter, stream, "subscribed", &subscription_id, data).await;
    }
    let mut events = state.subscribe();
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

pub async fn run() -> Result<()> {
    let state = StateStore::default();
    let api = ApiService::new(state.clone());
    let daemon = BarDaemon {
        api,
        state: state.clone(),
        sequence: AtomicU64::new(1),
        subscriptions: Mutex::new(HashMap::new()),
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
    let media_task = tokio::spawn(media::monitor(state));
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
