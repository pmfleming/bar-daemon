use std::sync::Arc;

use super::{error, success};
use crate::{activity::notifications::service::NotificationService, state::StateStore};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct DndRequest {
    enabled: bool,
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

#[derive(Clone)]
pub(super) struct NotificationApi {
    state: StateStore,
    notifications: Arc<NotificationService>,
}

impl NotificationApi {
    pub(super) fn new(state: StateStore, notifications: Arc<NotificationService>) -> Self {
        Self {
            state,
            notifications,
        }
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
        engine.set_dnd(request.enabled).await;
        success(json!({"notifications": self.state.snapshot().await.notifications}))
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
        if engine.dismiss(id).await {
            success(json!({"operation":"dismiss","id":id}))
        } else {
            error(
                "notification-not-found",
                format!("notification {id} is not active"),
            )
        }
    }
    pub(super) async fn notification_clear(&self) -> Value {
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        success(json!({"operation":"clear","closed":engine.clear().await}))
    }
    pub(super) async fn notification_invoke_action(&self, params: Value) -> Value {
        let request = request!(params, ActionRequest, "notifications.invokeAction");
        let Some(id) = positive_id(request.id) else {
            return error("validation-error", "notification id must be positive");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        if engine
            .invoke_action(id, &request.action_key, request.activation_token)
            .await
        {
            success(json!({"operation":"invoke-action","id":id,"action_key":request.action_key}))
        } else {
            error(
                "notification-action-not-found",
                "notification or action is unavailable",
            )
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
