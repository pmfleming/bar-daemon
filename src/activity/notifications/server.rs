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

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

#[derive(Clone)]
pub struct NotificationServer {
    engine: Arc<NotificationEngine>,
}

impl NotificationServer {
    pub fn new(engine: Arc<NotificationEngine>) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> Arc<NotificationEngine> {
        Arc::clone(&self.engine)
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
        self.engine.close(id, close_reason::CLOSED_BY_CALL).await;
        Ok(())
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
    pub async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;
}

pub async fn forward_signals(engine: Arc<NotificationEngine>, connection: Connection) {
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
    use std::collections::HashMap;

    use zvariant::OwnedValue;

    use super::normalize_hints;

    #[test]
    fn normalizes_common_notification_hints() {
        let mut hints = HashMap::new();
        hints.insert("urgency".into(), OwnedValue::from(2_u8));
        hints.insert("resident".into(), OwnedValue::from(true));
        let normalized = normalize_hints(&hints);
        assert_eq!(normalized.urgency, 2);
        assert!(normalized.resident);
    }
}
