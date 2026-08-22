use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use zvariant::OwnedValue;

use super::{
    engine::NotificationEngine,
    model::{IncomingNotification, NotificationHints},
    swaync,
};

#[derive(Clone)]
pub(crate) struct NotificationService {
    backend: NotificationBackend,
}

#[derive(Clone)]
enum NotificationBackend {
    Native(Arc<NotificationEngine>),
    SwayNc,
}

impl NotificationService {
    pub(crate) fn native(engine: Arc<NotificationEngine>) -> Arc<Self> {
        Arc::new(Self {
            backend: NotificationBackend::Native(engine),
        })
    }

    pub(crate) fn swaync() -> Arc<Self> {
        Arc::new(Self {
            backend: NotificationBackend::SwayNc,
        })
    }

    pub(crate) fn native_engine(&self) -> Option<Arc<NotificationEngine>> {
        match &self.backend {
            NotificationBackend::Native(engine) => Some(Arc::clone(engine)),
            NotificationBackend::SwayNc => None,
        }
    }

    pub(crate) fn sink(&self) -> NotificationSink {
        match &self.backend {
            NotificationBackend::Native(engine) => NotificationSink {
                backend: SinkBackend::Native(Arc::clone(engine)),
            },
            NotificationBackend::SwayNc => NotificationSink {
                backend: SinkBackend::Freedesktop,
            },
        }
    }

    pub(crate) async fn toggle_dnd(&self) -> Result<bool> {
        match &self.backend {
            NotificationBackend::Native(engine) => Ok(engine.toggle_dnd().await),
            NotificationBackend::SwayNc => {
                swaync::toggle_dnd().await?;
                Ok(false)
            }
        }
    }

    pub(crate) async fn toggle_panel(&self) -> Result<()> {
        match &self.backend {
            NotificationBackend::Native(_) => Ok(()),
            NotificationBackend::SwayNc => swaync::toggle_panel().await,
        }
    }
}

#[derive(Clone)]
pub(crate) struct NotificationSink {
    backend: SinkBackend,
}

#[derive(Clone)]
enum SinkBackend {
    Native(Arc<NotificationEngine>),
    Freedesktop,
    #[cfg(test)]
    Unavailable,
}

impl NotificationSink {
    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            backend: SinkBackend::Unavailable,
        }
    }

    pub(crate) async fn send(&self, notification: IncomingNotification) -> Result<u32> {
        match &self.backend {
            SinkBackend::Native(engine) => engine.notify(0, notification).await,
            SinkBackend::Freedesktop => send_freedesktop(notification).await,
            #[cfg(test)]
            SinkBackend::Unavailable => anyhow::bail!("notification sink is unavailable"),
        }
    }
}

async fn send_freedesktop(notification: IncomingNotification) -> Result<u32> {
    let connection = zbus::Connection::session()
        .await
        .context("connect to notification session bus")?;
    let proxy = zbus::Proxy::new(
        &connection,
        super::server::BUS_NAME,
        super::server::OBJECT_PATH,
        super::server::BUS_NAME,
    )
    .await
    .context("connect to notification server")?;
    let actions = notification
        .actions
        .iter()
        .flat_map(|action| [action.key.clone(), action.label.clone()])
        .collect::<Vec<_>>();
    let mut hints = HashMap::<String, OwnedValue>::new();
    hints.insert(
        "urgency".into(),
        OwnedValue::from(notification.hints.urgency),
    );
    if !notification.hints.category.is_empty() {
        hints.insert(
            "category".into(),
            OwnedValue::try_from(zvariant::Value::new(notification.hints.category.as_str()))
                .context("encode notification category")?,
        );
    }
    proxy
        .call(
            "Notify",
            &(
                notification.app_name.as_str(),
                0u32,
                notification.app_icon.as_str(),
                notification.summary.as_str(),
                notification.body.as_str(),
                actions,
                hints,
                notification.expire_timeout,
            ),
        )
        .await
        .context("send notification")
}

pub(crate) fn internal_notification(
    icon: &str,
    summary: &str,
    body: &str,
    urgency: u8,
) -> IncomingNotification {
    IncomingNotification {
        app_name: "bar-daemon".into(),
        app_icon: icon.into(),
        summary: summary.into(),
        body: body.into(),
        actions: Vec::new(),
        hints: NotificationHints {
            urgency: urgency.min(2),
            category: "device".into(),
            ..NotificationHints::default()
        },
        expire_timeout: -1,
    }
}
