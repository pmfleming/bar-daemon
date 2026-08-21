use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationHints {
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
pub struct IncomingNotification {
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub hints: NotificationHints,
    pub expire_timeout: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveNotification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    pub hints: NotificationHints,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub expires_unix_ms: Option<u64>,
    pub group_key: String,
}

impl ActiveNotification {
    pub fn from_incoming(id: u32, incoming: IncomingNotification, now: u64) -> Self {
        let timeout = effective_timeout_ms(incoming.expire_timeout, incoming.hints.urgency);
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
            app_name: incoming.app_name,
            app_icon: incoming.app_icon,
            summary: incoming.summary,
            body: incoming.body,
            actions: incoming.actions,
            hints: incoming.hints,
            created_unix_ms: now,
            updated_unix_ms: now,
            expires_unix_ms,
            group_key,
        }
    }

    pub fn replace_from(&mut self, incoming: IncomingNotification, now: u64) {
        let created = self.created_unix_ms;
        *self = Self::from_incoming(self.id, incoming, now);
        self.created_unix_ms = created;
    }
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
pub enum NotificationSignal {
    Closed { id: u32, reason: u32 },
    ActionInvoked { id: u32, action_key: String },
    ActivationToken { id: u32, token: String },
}

pub mod close_reason {
    pub const EXPIRED: u32 = 1;
    pub const DISMISSED: u32 = 2;
    pub const CLOSED_BY_CALL: u32 = 3;
    pub const UNDEFINED: u32 = 4;
}
