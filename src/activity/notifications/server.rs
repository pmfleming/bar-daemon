use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;
use zbus::{fdo, object_server::SignalEmitter};
use zvariant::OwnedValue;

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingNotification {
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<String>,
    pub expire_timeout: i32,
}

#[derive(Default)]
struct ServerState {
    next_id: u32,
    active: HashMap<u32, IncomingNotification>,
}

#[derive(Clone, Default)]
pub struct NotificationServer {
    state: Arc<Mutex<ServerState>>,
}

impl NotificationServer {
    pub fn new() -> Self {
        Self::default()
    }

    async fn notify_inner(&self, replaces_id: u32, notification: IncomingNotification) -> u32 {
        let mut state = self.state.lock().await;
        let id = if replaces_id != 0 && state.active.contains_key(&replaces_id) {
            replaces_id
        } else {
            next_notification_id(&mut state)
        };
        state.active.insert(id, notification);
        id
    }

    async fn close_inner(&self, id: u32) -> bool {
        self.state.lock().await.active.remove(&id).is_some()
    }

    #[cfg(test)]
    async fn active_count(&self) -> usize {
        self.state.lock().await.active.len()
    }
}

fn next_notification_id(state: &mut ServerState) -> u32 {
    loop {
        state.next_id = state.next_id.wrapping_add(1).max(1);
        if !state.active.contains_key(&state.next_id) {
            return state.next_id;
        }
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationServer {
    async fn get_capabilities(&self) -> Vec<String> {
        vec!["body".into()]
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
        _hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> fdo::Result<u32> {
        if actions.len() % 2 != 0 {
            return Err(fdo::Error::InvalidArgs(
                "notification actions must contain key/label pairs".into(),
            ));
        }
        Ok(self
            .notify_inner(
                replaces_id,
                IncomingNotification {
                    app_name: app_name.into(),
                    app_icon: app_icon.into(),
                    summary: summary.into(),
                    body: body.into(),
                    actions,
                    expire_timeout,
                },
            )
            .await)
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        if self.close_inner(id).await {
            Self::notification_closed(&emitter, id, 3).await?;
        }
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

#[cfg(test)]
mod tests {
    use super::{IncomingNotification, NotificationServer};

    fn notification(summary: &str) -> IncomingNotification {
        IncomingNotification {
            app_name: "test".into(),
            app_icon: String::new(),
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            expire_timeout: -1,
        }
    }

    #[tokio::test]
    async fn allocates_and_replaces_active_ids() {
        let server = NotificationServer::new();
        let first = server.notify_inner(0, notification("first")).await;
        let replaced = server
            .notify_inner(first, notification("replacement"))
            .await;
        let second = server.notify_inner(999, notification("second")).await;
        assert_eq!(first, replaced);
        assert_ne!(first, second);
        assert_eq!(server.active_count().await, 2);
    }
}
