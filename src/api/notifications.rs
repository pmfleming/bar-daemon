use super::{ApiService, error, success};
use serde_json::{Value, json};

impl ApiService {
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
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return error("validation-error", "notifications.setDnd requires enabled");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        engine.set_dnd(enabled).await;
        success(json!({"notifications": self.state.snapshot().await.notifications}))
    }
    pub(super) async fn notification_list(&self, params: Value) -> Value {
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        let before = match params.get("before_history_id") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_i64() {
                Some(value) if value > 0 => Some(value),
                _ => {
                    return error(
                        "validation-error",
                        "before_history_id must be positive or null",
                    );
                }
            },
        };
        let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50);
        let Ok(limit) = usize::try_from(limit.min(200)) else {
            return error("validation-error", "notification history limit is invalid");
        };
        match engine.history(before, limit).await {
            Ok(history) => success(json!({"notification_history": history})),
            Err(value) => error("notification-history-failed", value.to_string()),
        }
    }
    pub(super) async fn notification_dismiss(&self, params: Value) -> Value {
        let Some(id) = notification_id(&params) else {
            return error(
                "validation-error",
                "notifications.dismiss requires a positive id",
            );
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
        let Some(id) = notification_id(&params) else {
            return error(
                "validation-error",
                "notifications.invokeAction requires a positive id",
            );
        };
        let Some(key) = params.get("action_key").and_then(Value::as_str) else {
            return error(
                "validation-error",
                "notifications.invokeAction requires action_key",
            );
        };
        let token = params
            .get("activation_token")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        if engine.invoke_action(id, key, token).await {
            success(json!({"operation":"invoke-action","id":id,"action_key":key}))
        } else {
            error(
                "notification-action-not-found",
                "notification or action is unavailable",
            )
        }
    }
    pub(super) async fn notification_reply(&self, params: Value) -> Value {
        let Some(id) = notification_id(&params) else {
            return error(
                "validation-error",
                "notifications.reply requires a positive id",
            );
        };
        let Some(text) = params.get("text").and_then(Value::as_str) else {
            return error("validation-error", "notifications.reply requires text");
        };
        let Some(engine) = self.notifications.native_engine() else {
            return native_required();
        };
        if engine.reply(id, text).await {
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
fn notification_id(params: &Value) -> Option<u32> {
    params
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .filter(|id| *id > 0)
}
