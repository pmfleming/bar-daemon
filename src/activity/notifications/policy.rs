use anyhow::{Result, bail};

use super::model::IncomingNotification;
use serde::{Deserialize, Serialize};

// UTF-8 bytes, not characters. Keep both individual fields and the complete
// serialized payload bounded before cloning into snapshots or persisting.
const NAME_BYTES: usize = 1024;
const PATH_BYTES: usize = 4096;
const CATEGORY_BYTES: usize = 256;
const PAYLOAD_BYTES: usize = 128 * 1024;

fn bounded_text(field: &str, value: &str, maximum: usize) -> Result<()> {
    if value.len() > maximum {
        bail!("notification {field} exceeds {maximum}-byte size limit");
    }
    Ok(())
}

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
        for (field, value, maximum) in [
            ("app_name", &notification.app_name, NAME_BYTES),
            ("app_icon", &notification.app_icon, PATH_BYTES),
            ("category", &notification.hints.category, CATEGORY_BYTES),
            (
                "desktop_entry",
                &notification.hints.desktop_entry,
                NAME_BYTES,
            ),
            ("image_path", &notification.hints.image_path, PATH_BYTES),
            ("sound_name", &notification.hints.sound_name, NAME_BYTES),
            ("sound_file", &notification.hints.sound_file, PATH_BYTES),
        ] {
            bounded_text(field, value, maximum)?;
        }
        for action in &notification.actions {
            bounded_text("action key", &action.key, NAME_BYTES)?;
            bounded_text("action label", &action.label, PATH_BYTES)?;
        }
        if notification
            .actions
            .iter()
            .any(|action| action.key.is_empty())
        {
            bail!("notification action key cannot be empty");
        }
        // Per-field checks above bound this allocation, including JSON escaping.
        if serde_json::to_vec(notification)?.len() > PAYLOAD_BYTES {
            bail!("notification exceeds total {PAYLOAD_BYTES}-byte payload limit");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{NotificationAction, incoming};
    use super::*;

    #[test]
    fn ingress_bounds_utf8_metadata_actions_and_encoded_payloads() {
        type Field = fn(&mut IncomingNotification) -> &mut String;
        let fields: [(Field, usize); 9] = [
            (|n| &mut n.app_name, NAME_BYTES),
            (|n| &mut n.app_icon, PATH_BYTES),
            (|n| &mut n.hints.category, CATEGORY_BYTES),
            (|n| &mut n.hints.desktop_entry, NAME_BYTES),
            (|n| &mut n.hints.image_path, PATH_BYTES),
            (|n| &mut n.hints.sound_name, NAME_BYTES),
            (|n| &mut n.hints.sound_file, PATH_BYTES),
            (|n| &mut n.actions[0].key, NAME_BYTES),
            (|n| &mut n.actions[0].label, PATH_BYTES),
        ];
        for (field, maximum) in fields {
            let mut n = incoming("test", "summary", "body");
            n.actions.push(NotificationAction {
                key: "key".into(),
                label: "label".into(),
            });
            *field(&mut n) = "é".repeat(maximum / 2);
            assert!(NotificationPolicy::default().validate(&n).is_ok());
            field(&mut n).push('x');
            assert!(NotificationPolicy::default().validate(&n).is_err());
        }
        let policy = NotificationPolicy::default();
        let mut n = incoming("test", "summary", &"x".repeat(policy.maximum_body_bytes));
        assert!(policy.validate(&n).is_ok());
        for escaping in [false, true] {
            n.actions = if escaping {
                Vec::new()
            } else {
                vec![
                    NotificationAction {
                        key: "key".into(),
                        label: "x".repeat(2048)
                    };
                    64
                ]
            };
            n.body = if escaping { "\u{0001}" } else { "x" }.repeat(policy.maximum_body_bytes);
            assert!(
                policy
                    .validate(&n)
                    .unwrap_err()
                    .to_string()
                    .contains("total")
            );
        }
    }
}
