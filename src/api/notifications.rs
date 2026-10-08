use std::sync::Arc;

use super::{decode_request, error, success};
use crate::activity::notifications::{
    engine::NotificationEngine, history::HistoryError, service::NotificationService,
};
use serde::Deserialize;
use serde_json::{Value, json};

type Response = Result<Value, Value>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyRequest {
    app_key: String,
    policy: crate::activity::notifications::policy::AppPolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteScope {
    app_key: Option<String>,
    selected: Option<crate::activity::notifications::history::Position>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    token: String,
    #[serde(default)]
    cancel: bool,
}
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

    pub(super) async fn dispatch(&self, method: &str, params: Value) -> Value {
        let result = match method {
            "notifications.togglePanel" => self.action(false).await,
            "notifications.toggleDnd" => self.action(true).await,
            "notifications.setDnd" => self.set_dnd(params).await,
            "notifications.setAppPolicy" => self.set_app_policy(params).await,
            "notifications.prepareDelete" => self.prepare_delete(params).await,
            "notifications.delete" => self.delete(params).await,
            "notifications.list" => self.list(params).await,
            "notifications.queryCenter" => self.query_center(params).await,
            "notifications.queryHistory" => self.query_history(params).await,
            "notifications.dismiss" => self.dismiss(params).await,
            "notifications.clear" => self.clear().await,
            "notifications.clearGroup" => self.clear_group(params).await,
            "notifications.snooze" => self.snooze(params).await,
            "notifications.invokeAction" => self.invoke_action(params).await,
            "notifications.reply" => self.reply(params).await,
            _ => Err(super::unsupported_method(method)),
        };
        result.map_or_else(std::convert::identity, success)
    }

    fn engine(&self) -> Result<&Arc<NotificationEngine>, Value> {
        self.notifications.native_engine().ok_or_else(|| {
            error(
                "native-notifications-required",
                "native notifications are disabled",
            )
        })
    }

    async fn action(&self, dnd: bool) -> Response {
        if dnd {
            let enabled = self
                .notifications
                .toggle_dnd()
                .await
                .map_err(operation_error)?;
            Ok(json!({"operation":"toggle-dnd", "enabled":enabled}))
        } else {
            self.notifications
                .toggle_panel()
                .await
                .map_err(operation_error)?;
            Ok(json!({"operation":"toggle-panel"}))
        }
    }
    async fn set_app_policy(&self, params: Value) -> Response {
        let request: PolicyRequest = decode_request(params, "notifications.setAppPolicy")?;
        let state = self
            .engine()?
            .set_app_policy(request.app_key, request.policy)
            .await
            .map_err(|e| error("notification-policy-failed", e.to_string()))?;
        Ok(json!({"notifications":state}))
    }
    async fn prepare_delete(&self, params: Value) -> Response {
        let request: DeleteScope = decode_request(params, "notifications.prepareDelete")?;
        let challenge = self
            .engine()?
            .prepare_delete(request.app_key, request.selected)
            .await
            .map_err(delete_error)?;
        Ok(json!({"delete_confirmation":challenge}))
    }
    async fn delete(&self, params: Value) -> Response {
        let request: DeleteRequest = decode_request(params, "notifications.delete")?;
        let engine = self.engine()?;
        if request.cancel {
            engine.cancel_delete(&request.token).await;
            return Ok(json!({"cancelled":true}));
        }
        let count = engine
            .delete_confirmed(request.token)
            .await
            .map_err(delete_error)?;
        Ok(json!({"deleted":count}))
    }
    async fn set_dnd(&self, params: Value) -> Response {
        let request: DndRequest = decode_request(params, "notifications.setDnd")?;
        let state = self
            .engine()?
            .set_dnd(request.enabled, request.until_unix_ms)
            .await
            .map_err(operation_error)?;
        Ok(json!({"notifications":state}))
    }
    async fn query_center(&self, params: Value) -> Response {
        let engine = self.engine()?;
        let request = decode_request(params, "notifications.queryCenter")?;
        let page = engine.query_center(request).await.map_err(history_error)?;
        Ok(json!({"notification_center":page}))
    }
    async fn query_history(&self, params: Value) -> Response {
        let engine = self.engine()?;
        let request = decode_request(params, "notifications.queryHistory")?;
        let page = engine.query_history(request).await.map_err(history_error)?;
        Ok(json!({"notification_page":page}))
    }
    async fn list(&self, params: Value) -> Response {
        let engine = self.engine()?;
        let request: HistoryRequest = decode_request(params, "notifications.list")?;
        if request.before_history_id.is_some_and(|id| id <= 0) {
            return Err(error(
                "validation-error",
                "before_history_id must be positive or null",
            ));
        }
        let history = engine
            .history(request.before_history_id, request.limit.min(200))
            .await
            .map_err(|e| error("notification-history-failed", e.to_string()))?;
        Ok(json!({"notification_history":history}))
    }
    async fn dismiss(&self, params: Value) -> Response {
        let request: NotificationRequest = decode_request(params, "notifications.dismiss")?;
        let id = positive_id(request.id)?;
        if self.engine()?.dismiss(id).await.map_err(operation_error)? {
            Ok(json!({"operation":"dismiss", "id":id}))
        } else {
            Err(not_found(id))
        }
    }
    async fn clear(&self) -> Response {
        let closed = self.engine()?.clear().await.map_err(operation_error)?;
        Ok(json!({"operation":"clear", "closed":closed}))
    }
    async fn clear_group(&self, params: Value) -> Response {
        let request: GroupRequest = decode_request(params, "notifications.clearGroup")?;
        if request.group_key.trim().is_empty() {
            return Err(error("validation-error", "group_key must not be empty"));
        }
        let closed = self
            .engine()?
            .clear_group(&request.group_key)
            .await
            .map_err(operation_error)?;
        Ok(json!({"operation":"clear-group", "group_key":request.group_key, "closed":closed}))
    }
    async fn snooze(&self, params: Value) -> Response {
        let request: SnoozeRequest = decode_request(params, "notifications.snooze")?;
        let id = positive_id(request.id)?;
        if self
            .engine()?
            .snooze(id, request.until_unix_ms)
            .await
            .map_err(operation_error)?
        {
            Ok(json!({"operation":"snooze", "id":id, "until_unix_ms":request.until_unix_ms}))
        } else {
            Err(error(
                "notification-snooze-failed",
                "notification is unavailable or duration has elapsed",
            ))
        }
    }
    async fn invoke_action(&self, params: Value) -> Response {
        let request: ActionRequest = decode_request(params, "notifications.invokeAction")?;
        let id = positive_id(request.id)?;
        if self
            .engine()?
            .invoke_action(id, &request.action_key, request.activation_token)
            .await
            .map_err(operation_error)?
        {
            Ok(json!({"operation":"invoke-action", "id":id, "action_key":request.action_key}))
        } else {
            Err(error(
                "notification-action-not-found",
                "notification or action is unavailable",
            ))
        }
    }
    async fn reply(&self, params: Value) -> Response {
        let request: ReplyRequest = decode_request(params, "notifications.reply")?;
        let id = positive_id(request.id)?;
        if self.engine()?.reply(id, &request.text).await {
            Ok(json!({"operation":"reply", "id":id}))
        } else {
            Err(not_found(id))
        }
    }
}
fn positive_id(id: u32) -> Result<u32, Value> {
    (id > 0)
        .then_some(id)
        .ok_or_else(|| error("validation-error", "notification id must be positive"))
}
fn not_found(id: u32) -> Value {
    error(
        "notification-not-found",
        format!("notification {id} is not active"),
    )
}
fn operation_error(value: anyhow::Error) -> Value {
    error("notification-operation-failed", value.to_string())
}
fn delete_error(value: anyhow::Error) -> Value {
    error("notification-delete-failed", value.to_string())
}
fn history_error(value: HistoryError) -> Value {
    error(value.code(), value.message())
}

#[cfg(test)]
mod tests {
    use super::{NotificationApi, NotificationEngine, NotificationService};
    use serde_json::json;

    #[tokio::test]
    async fn native_responses_have_one_envelope_and_preserve_domain_errors() {
        let engine = NotificationEngine::new(Default::default()).await;
        let api = NotificationApi::new(NotificationService::native(engine));
        for (method, params, field) in [
            ("queryCenter", json!({"view":"apps"}), "notification_center"),
            ("queryHistory", json!({}), "notification_page"),
            ("setDnd", json!({"enabled":true}), "notifications"),
            ("clear", json!({}), "closed"),
        ] {
            let response = api
                .dispatch(&format!("notifications.{method}"), params)
                .await;
            assert_eq!(response["ok"], true, "{response}");
            assert!(response["data"].get(field).is_some(), "{response}");
        }
        assert_eq!(
            api.dispatch("notifications.dismiss", json!({"id":1})).await["error"]["code"],
            "notification-not-found"
        );
        assert_eq!(
            api.dispatch("notifications.queryHistory", json!({"limit":0}))
                .await["error"]["code"],
            "history-query-invalid"
        );
    }
}
