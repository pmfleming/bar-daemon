use anyhow::{Result, bail};

use super::model::IncomingNotification;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AppPolicy {
    pub silent: bool,
    pub until_unix_ms: Option<u64>,
    pub group_similar: bool,
    pub bypass_dnd: bool,
}
impl Default for AppPolicy {
    fn default() -> Self {
        Self {
            silent: false,
            until_unix_ms: None,
            group_similar: true,
            bypass_dnd: false,
        }
    }
}
impl AppPolicy {
    pub fn silenced(&self, now: u64) -> bool {
        self.silent && self.until_unix_ms.is_none_or(|until| until > now)
    }
    pub fn validate(&self) -> Result<()> {
        if self.until_unix_ms.is_some_and(|until| {
            until <= crate::time::unix_ms()
                || until > crate::time::unix_ms().saturating_add(7 * 86_400_000)
        }) {
            bail!("quiet period must end within the next seven days");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NotificationPolicy {
    pub maximum_active: usize,
    pub maximum_summary_bytes: usize,
    pub maximum_body_bytes: usize,
    pub maximum_actions: usize,
}

impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            maximum_active: 200,
            maximum_summary_bytes: 4 * 1024,
            maximum_body_bytes: 64 * 1024,
            maximum_actions: 64,
        }
    }
}

impl NotificationPolicy {
    pub(crate) fn validate(&self, notification: &IncomingNotification) -> Result<()> {
        if notification.summary.len() > self.maximum_summary_bytes {
            bail!("notification summary exceeds configured size limit");
        }
        if notification.body.len() > self.maximum_body_bytes {
            bail!("notification body exceeds configured size limit");
        }
        if notification.actions.len() > self.maximum_actions {
            bail!("notification has too many actions");
        }
        if notification
            .actions
            .iter()
            .any(|action| action.key.is_empty())
        {
            bail!("notification action key cannot be empty");
        }
        Ok(())
    }
}
