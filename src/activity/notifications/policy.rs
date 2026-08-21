use anyhow::{Result, bail};

use super::model::IncomingNotification;

#[derive(Debug, Clone)]
pub struct NotificationPolicy {
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
    pub fn validate(&self, notification: &IncomingNotification) -> Result<()> {
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
