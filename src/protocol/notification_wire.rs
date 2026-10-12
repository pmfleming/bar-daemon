//! v1 notification mutation DTOs. Keep legacy request names and error messages.
use crate::{
    activity::notifications::{
        history::Position,
        model::{DeleteConfirmation, NotificationState},
        policy::AppPolicy,
    },
    api,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyRequest {
    pub app_key: String,
    pub policy: AppPolicy,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteScope {
    pub app_key: Option<String>,
    pub selected: Option<Position>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteRequest {
    pub token: String,
    #[serde(default)]
    pub cancel: bool,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct DndRequest {
    pub enabled: bool,
    #[serde(default)]
    pub until_unix_ms: Option<u64>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct NotificationRequest {
    pub id: u32,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct SnoozeRequest {
    pub id: u32,
    pub until_unix_ms: u64,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct GroupRequest {
    pub group_key: String,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct ActionRequest {
    pub id: u32,
    pub action_key: String,
    pub activation_token: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct ReplyRequest {
    pub id: u32,
    pub text: String,
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub(crate) enum Operation {
    TogglePanel,
    ToggleDnd { enabled: bool },
    Dismiss { id: u32 },
    Clear { closed: usize },
    ClearGroup { group_key: String, closed: usize },
    Snooze { id: u32, until_unix_ms: u64 },
    InvokeAction { id: u32, action_key: String },
    Reply { id: u32 },
}
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Reply<'a> {
    Operation(Operation),
    Notifications {
        notifications: &'a NotificationState,
    },
    DeleteConfirmation {
        delete_confirmation: &'a DeleteConfirmation,
    },
    Cancelled {
        cancelled: bool,
    },
    Deleted {
        deleted: usize,
    },
}
impl Reply<'_> {
    /// Body only: NotificationApi owns the single outer success envelope.
    pub(crate) fn body(self) -> Value {
        serde_json::to_value(self).expect("serializable notification reply")
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ErrorCode {
    NativeRequired,
    PolicyFailed,
    DeleteFailed,
    OperationFailed,
    NotFound,
    SnoozeFailed,
    ActionNotFound,
    Validation,
}
impl ErrorCode {
    pub(crate) fn reply(self, message: impl Into<String>) -> Value {
        api::error(
            match self {
                Self::NativeRequired => "native-notifications-required",
                Self::PolicyFailed => "notification-policy-failed",
                Self::DeleteFailed => "notification-delete-failed",
                Self::OperationFailed => "notification-operation-failed",
                Self::NotFound => "notification-not-found",
                Self::SnoozeFailed => "notification-snooze-failed",
                Self::ActionNotFound => "notification-action-not-found",
                Self::Validation => "validation-error",
            },
            message,
        )
    }
}
pub(crate) fn positive_id(id: u32) -> Result<u32, Value> {
    (id > 0)
        .then_some(id)
        .ok_or_else(|| ErrorCode::Validation.reply("notification id must be positive"))
}
pub(crate) fn not_found(id: u32) -> Value {
    ErrorCode::NotFound.reply(format!("notification {id} is not active"))
}
pub(crate) fn operation_error(error: anyhow::Error) -> Value {
    ErrorCode::OperationFailed.reply(error.to_string())
}
pub(crate) fn delete_error(error: anyhow::Error) -> Value {
    ErrorCode::DeleteFailed.reply(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn operation_tags_bodies_and_error_formatting_preserve_v1() {
        for (operation, expected) in [
            (Operation::TogglePanel, json!({"operation":"toggle-panel"})),
            (
                Operation::ToggleDnd { enabled: true },
                json!({"operation":"toggle-dnd", "enabled":true}),
            ),
            (
                Operation::Dismiss { id: 1 },
                json!({"operation":"dismiss", "id":1}),
            ),
            (
                Operation::Clear { closed: 1 },
                json!({"operation":"clear", "closed":1}),
            ),
            (
                Operation::ClearGroup {
                    group_key: "chat".into(),
                    closed: 1,
                },
                json!({"operation":"clear-group", "group_key":"chat", "closed":1}),
            ),
            (
                Operation::Snooze {
                    id: 1,
                    until_unix_ms: 42,
                },
                json!({"operation":"snooze", "id":1, "until_unix_ms":42}),
            ),
            (
                Operation::InvokeAction {
                    id: 1,
                    action_key: "open".into(),
                },
                json!({"operation":"invoke-action", "id":1, "action_key":"open"}),
            ),
            (
                Operation::Reply { id: 1 },
                json!({"operation":"reply", "id":1}),
            ),
        ] {
            assert_eq!(Reply::Operation(operation).body(), expected);
        }
        assert_eq!(
            Reply::Cancelled { cancelled: true }.body(),
            json!({"cancelled":true})
        );
        assert_eq!(Reply::Deleted { deleted: 1 }.body(), json!({"deleted":1}));
        assert_eq!(
            operation_error(anyhow::anyhow!("cause").context("context")),
            api::error("notification-operation-failed", "context")
        );
        assert_eq!(
            delete_error(anyhow::anyhow!("cause").context("context")),
            api::error("notification-delete-failed", "context")
        );
        assert_eq!(
            positive_id(0).unwrap_err(),
            ErrorCode::Validation.reply("notification id must be positive")
        );
    }
}
