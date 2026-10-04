use std::sync::Arc;

use super::{error, success};
use crate::activity::notifications::service::NotificationService;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct DndRequest {
    enabled: bool,
    #[serde(default)]
    until_unix_ms: Option<u64>,
}

#[derive(Deserialize)]
struct HistoryRequest {
    before_history_id: Option<i64>,
    #[serde(default = "default_history_limit")]
    limit: usize,
}

#[derive(Deserialize)]
struct NotificationRequest {
    id: u32,
}

#[derive(Deserialize)]
struct SnoozeRequest {
    id: u32,
    until_unix_ms: u64,
}

#[derive(Deserialize)]
struct GroupRequest {
    group_key: String,
}

#[derive(Deserialize)]
struct ActionRequest {
    id: u32,
    action_key: String,
    activation_token: Option<String>,
}

#[derive(Deserialize)]
struct ReplyRequest {
    id: u32,
    text: String,
}

const fn default_history_limit() -> usize {
    50
}

pub(super) struct NotificationApi {
    notifications: Arc<NotificationService>,
}

impl NotificationApi {
    pub(super) fn new(notifications: Arc<NotificationService>) -> Self {
        Self { notifications }
    }

    pub(super) async fn notification_action(&self, dnd: bool) -> Value {
        let result = if dnd {
            self.notifications
                .toggle_dnd()
                .await
                .map(|enabled| json!({"operation":"toggle-dnd","enabled":enabled}))
        } else {
            self.notifications
                .toggle_panel()
                .await
                .map(|()| json!({"operation":"toggle-panel"}))
        };
        match result {
            Ok(operation) => success(operation),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_set_dnd(&self, params: Value) -> Value {
        let request = request!(params, DndRequest, "notifications.setDnd");
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        match engine.set_dnd(request.enabled, request.until_unix_ms).await {
            Ok(state) => success(json!({"notifications": state})),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_query_history(&self, params: Value) -> Value {
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        let request = request!(
            params,
            crate::activity::notifications::history::HistoryQuery,
            "notifications.queryHistory"
        );
        match engine.query_history(request).await {
            Ok(page) => success(json!({"notification_page": page})),
            Err(value) => error(value.code(), value.message()),
        }
    }
    pub(super) async fn notification_list(&self, params: Value) -> Value {
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        let request = request!(params, HistoryRequest, "notifications.list");
        if request.before_history_id.is_some_and(|id| id <= 0) {
            return error(
                "validation-error",
                "before_history_id must be positive or null",
            );
        }
        match engine
            .history(request.before_history_id, request.limit.min(200))
            .await
        {
            Ok(history) => success(json!({"notification_history": history})),
            Err(value) => error("notification-history-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_dismiss(&self, params: Value) -> Value {
        let request = request!(params, NotificationRequest, "notifications.dismiss");
        let Some(id) = positive_id(request.id) else {
            return error("validation-error", "notification id must be positive");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        match engine.dismiss(id).await {
            Ok(true) => success(json!({"operation":"dismiss","id":id})),
            Ok(false) => error(
                "notification-not-found",
                format!("notification {id} is not active"),
            ),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_clear(&self) -> Value {
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        match engine.clear().await {
            Ok(closed) => success(json!({"operation":"clear","closed":closed})),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_clear_group(&self, params: Value) -> Value {
        let request = request!(params, GroupRequest, "notifications.clearGroup");
        if request.group_key.trim().is_empty() {
            return error("validation-error", "group_key must not be empty");
        }
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        match engine.clear_group(&request.group_key).await {
            Ok(closed) => success(
                json!({"operation":"clear-group", "group_key":request.group_key, "closed":closed}),
            ),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_snooze(&self, params: Value) -> Value {
        let request = request!(params, SnoozeRequest, "notifications.snooze");
        let Some(id) = positive_id(request.id) else {
            return error("validation-error", "notification id must be positive");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        match engine.snooze(id, request.until_unix_ms).await {
            Ok(true) => success(
                json!({"operation":"snooze", "id":id, "until_unix_ms":request.until_unix_ms}),
            ),
            Ok(false) => error(
                "notification-snooze-failed",
                "notification is unavailable or duration has elapsed",
            ),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_invoke_action(&self, params: Value) -> Value {
        let request = request!(params, ActionRequest, "notifications.invokeAction");
        let Some(id) = positive_id(request.id) else {
            return error("validation-error", "notification id must be positive");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        match engine
            .invoke_action(id, &request.action_key, request.activation_token)
            .await
        {
            Ok(true) => success(
                json!({"operation":"invoke-action","id":id,"action_key":request.action_key}),
            ),
            Ok(false) => error(
                "notification-action-not-found",
                "notification or action is unavailable",
            ),
            Err(value) => error("notification-operation-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_reply(&self, params: Value) -> Value {
        let request = request!(params, ReplyRequest, "notifications.reply");
        let Some(id) = positive_id(request.id) else {
            return error("validation-error", "notification id must be positive");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        if engine.reply(id, &request.text).await {
            success(json!({"operation":"reply","id":id}))
        } else {
            error(
                "notification-not-found",
                format!("notification {id} is not active"),
            )
        }
    }
}
fn native_required() -> Value {
    error(
        "native-notifications-required",
        "native notifications are disabled",
    )
}
fn positive_id(id: u32) -> Option<u32> {
    (id > 0).then_some(id)
}
