use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NotificationActiveState {
    pub available: bool,
    pub revision: u64,
    pub notifications: Vec<ActiveNotification>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NotificationState {
    pub available: bool,
    pub count: u32,
    pub dnd: bool,
    pub dnd_until_unix_ms: Option<u64>,
    pub inhibited: bool,
    pub text: String,
    pub tooltip: String,
    pub alt: String,
    pub class_name: String,
    pub backend: String,
    pub history_revision: u64,
    #[serde(default)]
    pub app_policies: std::collections::BTreeMap<String, super::policy::AppPolicy>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NotificationHints {
    pub urgency: u8,
    pub category: String,
    pub desktop_entry: String,
    pub image_path: String,
    pub sound_name: String,
    pub sound_file: String,
    pub resident: bool,
    pub transient: bool,
    pub suppress_sound: bool,
    pub image_data_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IncomingNotification {
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub hints: NotificationHints,
    pub expire_timeout: i32,
}

#[cfg(test)]
pub(super) fn incoming(app: &str, summary: &str, body: &str) -> IncomingNotification {
    IncomingNotification {
        app_name: app.into(),
        app_icon: String::new(),
        summary: summary.into(),
        body: body.into(),
        actions: Vec::new(),
        hints: NotificationHints::default(),
        expire_timeout: 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActiveNotification {
    pub id: u32,
    #[serde(default)]
    pub identity_icon: String,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub hints: NotificationHints,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    /// Protocol lifetime; None keeps the notification actionable in the center.
    pub expires_unix_ms: Option<u64>,
    #[serde(default = "default_toast_visible")]
    pub toast_visible: bool,
    #[serde(default)]
    pub dnd_bypass: bool,
    #[serde(default)]
    pub toast_expires_unix_ms: Option<u64>,
    pub group_key: String,
    #[serde(default)]
    pub source_monitor: String,
    #[serde(default)]
    pub snoozed_until_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HistoryNotification {
    pub history_id: i64,
    pub notification: ActiveNotification,
    pub closed_unix_ms: Option<u64>,
    pub close_reason: Option<u32>,
}

impl ActiveNotification {
    pub(super) fn app_key(&self) -> String {
        super::identity::app_key(
            &self.hints.desktop_entry,
            &self.app_name,
            self.id,
            self.created_unix_ms,
        )
    }

    pub(crate) fn from_incoming(id: u32, incoming: IncomingNotification, now: u64) -> Self {
        let toast_timeout = effective_timeout_ms(incoming.expire_timeout, incoming.hints.urgency);
        // Default, non-transient notifications persist in the center. Explicit
        // client expiry and transient notifications still close on timeout.
        let timeout = if incoming.expire_timeout < 0 && !incoming.hints.transient {
            None
        } else {
            toast_timeout
        };
        let expires_unix_ms = timeout.map(|timeout| now.saturating_add(timeout));
        let group_key = if !incoming.hints.desktop_entry.is_empty() {
            incoming.hints.desktop_entry.clone()
        } else if !incoming.app_name.is_empty() {
            incoming.app_name.clone()
        } else {
            "unknown".into()
        };
        Self {
            id,
            identity_icon: super::identity::icon(&incoming.hints.desktop_entry, &incoming.app_name),
            app_name: incoming.app_name,
            app_icon: incoming.app_icon,
            summary: incoming.summary,
            body: incoming.body,
            actions: incoming.actions,
            hints: incoming.hints,
            created_unix_ms: now,
            updated_unix_ms: now,
            expires_unix_ms,
            toast_visible: true,
            dnd_bypass: false,
            toast_expires_unix_ms: toast_timeout.map(|timeout| now.saturating_add(timeout)),
            group_key,
            source_monitor: String::new(),
            snoozed_until_unix_ms: None,
        }
    }

    pub(crate) fn replace_from(&mut self, incoming: IncomingNotification, now: u64) {
        let created = self.created_unix_ms;
        *self = Self::from_incoming(self.id, incoming, now);
        self.created_unix_ms = created;
    }
}

fn default_toast_visible() -> bool {
    true
}

fn effective_timeout_ms(requested: i32, urgency: u8) -> Option<u64> {
    match requested {
        0 => None,
        value if value > 0 => Some(value as u64),
        _ if urgency >= 2 => None,
        _ => Some(5_000),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotificationSignal {
    Closed { id: u32, reason: u32 },
    ActionInvoked { id: u32, action_key: String },
    ActivationToken { id: u32, token: String },
    Replied { id: u32, text: String },
}

pub(crate) mod close_reason {
    pub(crate) const EXPIRED: u32 = 1;
    pub(crate) const DISMISSED: u32 = 2;
    pub(crate) const CLOSED_BY_CALL: u32 = 3;
    pub(crate) const UNDEFINED: u32 = 4;
}
