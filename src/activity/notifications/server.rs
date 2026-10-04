use std::{collections::HashMap, sync::Arc};

use zbus::{Connection, fdo, object_server::SignalEmitter};
use zvariant::OwnedValue;

use super::{
    engine::NotificationEngine,
    model::{
        IncomingNotification, NotificationAction, NotificationHints, NotificationSignal,
        close_reason,
    },
};

pub(crate) const BUS_NAME: &str = "org.freedesktop.Notifications";
pub(crate) const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

#[derive(Clone)]
pub(crate) struct NotificationServer {
    engine: Arc<NotificationEngine>,
}

impl NotificationServer {
    pub(crate) fn new(engine: Arc<NotificationEngine>) -> Self {
        Self { engine }
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationServer {
    async fn get_capabilities(&self) -> Vec<String> {
        vec![
            "actions".into(),
            "action-icons".into(),
            "body".into(),
            "body-markup".into(),
            "icon-static".into(),
            "inline-reply".into(),
            "persistence".into(),
        ]
    }

    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> fdo::Result<u32> {
        if actions.len() % 2 != 0 {
            return Err(fdo::Error::InvalidArgs(
                "notification actions must contain key/label pairs".into(),
            ));
        }
        let actions = actions
            .chunks_exact(2)
            .map(|pair| NotificationAction {
                key: pair[0].clone(),
                label: pair[1].clone(),
            })
            .collect::<Vec<_>>();
        self.engine
            .notify(
                replaces_id,
                IncomingNotification {
                    app_name: app_name.into(),
                    app_icon: app_icon.into(),
                    summary: summary.into(),
                    body: body.into(),
                    actions,
                    hints: normalize_hints(&hints),
                    expire_timeout,
                },
            )
            .await
            .map_err(|error| fdo::Error::Failed(error.to_string()))
    }

    async fn close_notification(&self, id: u32) -> fdo::Result<()> {
        self.engine
            .close(id, close_reason::CLOSED_BY_CALL)
            .await
            .map(|_| ())
            .map_err(|error| fdo::Error::Failed(error.to_string()))
    }

    async fn get_server_information(&self) -> (String, String, String, String) {
        (
            "bar-daemon".into(),
            "laufan".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    #[zbus(signal)]
    pub(crate) async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(crate) async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(crate) async fn notification_replied(
        emitter: &SignalEmitter<'_>,
        id: u32,
        text: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(crate) async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;
}

pub(crate) async fn forward_signals(engine: Arc<NotificationEngine>, connection: Connection) {
    let mut signals = engine.subscribe_signals();
    loop {
        let signal = match signals.recv().await {
            Ok(signal) => signal,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                tracing::warn!(count, "notification D-Bus signal dispatcher lagged");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let result = match signal {
            NotificationSignal::Closed { id, reason } => {
                connection
                    .emit_signal(
                        None::<()>,
                        OBJECT_PATH,
                        INTERFACE,
                        "NotificationClosed",
                        &(id, reason),
                    )
                    .await
            }
            NotificationSignal::ActionInvoked { id, action_key } => {
                connection
                    .emit_signal(
                        None::<()>,
                        OBJECT_PATH,
                        INTERFACE,
                        "ActionInvoked",
                        &(id, action_key),
                    )
                    .await
            }
            NotificationSignal::ActivationToken { id, token } => {
                connection
                    .emit_signal(
                        None::<()>,
                        OBJECT_PATH,
                        INTERFACE,
                        "ActivationToken",
                        &(id, token),
                    )
                    .await
            }
            NotificationSignal::Replied { id, text } => {
                connection
                    .emit_signal(
                        None::<()>,
                        OBJECT_PATH,
                        INTERFACE,
                        "NotificationReplied",
                        &(id, text),
                    )
                    .await
            }
        };
        if let Err(error) = result {
            tracing::warn!(%error, "notification D-Bus signal could not be emitted");
        }
    }
}

fn normalize_hints(hints: &HashMap<String, OwnedValue>) -> NotificationHints {
    NotificationHints {
        urgency: hint_u8(hints, "urgency").unwrap_or(1).min(2),
        category: hint_string(hints, "category"),
        desktop_entry: hint_string(hints, "desktop-entry"),
        image_path: hint_string(hints, "image-path")
            .or_else_empty(|| hint_string(hints, "image_path")),
        sound_name: hint_string(hints, "sound-name"),
        sound_file: hint_string(hints, "sound-file"),
        resident: hint_bool(hints, "resident"),
        transient: hint_bool(hints, "transient"),
        suppress_sound: hint_bool(hints, "suppress-sound"),
        image_data_present: hints.contains_key("image-data") || hints.contains_key("image_data"),
    }
}

trait EmptyStringFallback {
    fn or_else_empty(self, fallback: impl FnOnce() -> String) -> String;
}

impl EmptyStringFallback for String {
    fn or_else_empty(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() { fallback() } else { self }
    }
}

fn hint_string(hints: &HashMap<String, OwnedValue>, key: &str) -> String {
    hints
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .unwrap_or_default()
        .into()
}

fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn hint_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    hints.get(key).and_then(|value| u8::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::time::{Duration, timeout};

    // A private peer connection exercises real D-Bus serialization/dispatch,
    // without owning the user's session-bus name or touching their notifications.
    #[tokio::test]
    async fn client_receives_actions_tokens_replies_and_closure() {
        let engine = NotificationEngine::new(crate::state::StateStore::default()).await;
        let (server, client) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(server)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(OBJECT_PATH, NotificationServer::new(Arc::clone(&engine)))
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(client).p2p().build();
        let (server, client) = tokio::try_join!(server, client).unwrap();
        let forwarder = tokio::spawn(forward_signals(Arc::clone(&engine), server.clone()));
        let proxy = zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, INTERFACE)
            .await
            .unwrap();
        let mut invoked = proxy.receive_signal("ActionInvoked").await.unwrap();
        let mut tokens = proxy.receive_signal("ActivationToken").await.unwrap();
        let mut replies = proxy.receive_signal("NotificationReplied").await.unwrap();
        let mut closed = proxy.receive_signal("NotificationClosed").await.unwrap();
        let hints = HashMap::from([("resident", OwnedValue::from(true))]);
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    "test",
                    0_u32,
                    "",
                    "Message",
                    "Body",
                    vec![
                        "default",
                        "Open",
                        "mail-reply-sender",
                        "Reply in app",
                        "inline-reply",
                        "Reply here",
                    ],
                    hints,
                    -1_i32,
                ),
            )
            .await
            .unwrap();
        for key in ["default", "mail-reply-sender"] {
            assert!(
                engine
                    .invoke_action(id, key, Some("test-token".into()))
                    .await
                    .unwrap()
            );
            let signal = timeout(Duration::from_secs(1), tokens.next())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                signal.body().deserialize::<(u32, String)>().unwrap(),
                (id, "test-token".into())
            );
            let signal = timeout(Duration::from_secs(1), invoked.next())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                signal.body().deserialize::<(u32, String)>().unwrap(),
                (id, key.into())
            );
        }
        assert!(engine.reply(id, "Hello").await);
        let signal = timeout(Duration::from_secs(1), replies.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            signal.body().deserialize::<(u32, String)>().unwrap(),
            (id, "Hello".into())
        );
        proxy
            .call::<_, _, ()>("CloseNotification", &(id,))
            .await
            .unwrap();
        let signal = timeout(Duration::from_secs(1), closed.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            signal.body().deserialize::<(u32, u32)>().unwrap(),
            (id, close_reason::CLOSED_BY_CALL)
        );
        assert!(!engine.invoke_action(id, "default", None).await.unwrap());
        assert!(!engine.reply(id, "No longer live").await);
        forwarder.abort();
    }
}
