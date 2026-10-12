use std::sync::Arc;

use super::{decode_request, error, success};
use crate::activity::notifications::{
    engine::NotificationEngine, history::HistoryError, service::NotificationService,
};
use crate::protocol::notification_wire::{
    ActionRequest, DeleteRequest, DeleteScope, DndRequest, ErrorCode, GroupRequest,
    NotificationRequest, Operation, PolicyRequest, Reply, ReplyRequest, SnoozeRequest,
    delete_error, not_found, operation_error, positive_id,
};
use serde::Deserialize;
use serde_json::{Value, json};

type Response = Result<Value, Value>;

#[derive(Deserialize)]
struct HistoryRequest {
    before_history_id: Option<i64>,
    #[serde(default = "default_history_limit")]
    limit: usize,
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
        self.notifications
            .native_engine()
            .ok_or_else(|| ErrorCode::NativeRequired.reply("native notifications are disabled"))
    }

    async fn action(&self, dnd: bool) -> Response {
        if dnd {
            let enabled = self
                .notifications
                .toggle_dnd()
                .await
                .map_err(operation_error)?;
            Ok(Reply::Operation(Operation::ToggleDnd { enabled }).body())
        } else {
            self.notifications
                .toggle_panel()
                .await
                .map_err(operation_error)?;
            Ok(Reply::Operation(Operation::TogglePanel).body())
        }
    }
    async fn set_app_policy(&self, params: Value) -> Response {
        let request: PolicyRequest = decode_request(params, "notifications.setAppPolicy")?;
        let state = self
            .engine()?
            .set_app_policy(request.app_key, request.policy)
            .await
            .map_err(|e| ErrorCode::PolicyFailed.reply(e.to_string()))?;
        Ok(Reply::Notifications {
            notifications: &state,
        }
        .body())
    }
    async fn prepare_delete(&self, params: Value) -> Response {
        let request: DeleteScope = decode_request(params, "notifications.prepareDelete")?;
        let challenge = self
            .engine()?
            .prepare_delete(request.app_key, request.selected)
            .await
            .map_err(delete_error)?;
        Ok(Reply::DeleteConfirmation {
            delete_confirmation: &challenge,
        }
        .body())
    }
    async fn delete(&self, params: Value) -> Response {
        let request: DeleteRequest = decode_request(params, "notifications.delete")?;
        let engine = self.engine()?;
        if request.cancel {
            engine.cancel_delete(&request.token).await;
            return Ok(Reply::Cancelled { cancelled: true }.body());
        }
        let count = engine
            .delete_confirmed(request.token)
            .await
            .map_err(delete_error)?;
        Ok(Reply::Deleted { deleted: count }.body())
    }
    async fn set_dnd(&self, params: Value) -> Response {
        let request: DndRequest = decode_request(params, "notifications.setDnd")?;
        let state = self
            .engine()?
            .set_dnd(request.enabled, request.until_unix_ms)
            .await
            .map_err(operation_error)?;
        Ok(Reply::Notifications {
            notifications: &state,
        }
        .body())
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
            Ok(Reply::Operation(Operation::Dismiss { id }).body())
        } else {
            Err(not_found(id))
        }
    }
    async fn clear(&self) -> Response {
        let closed = self.engine()?.clear().await.map_err(operation_error)?;
        Ok(Reply::Operation(Operation::Clear { closed }).body())
    }
    async fn clear_group(&self, params: Value) -> Response {
        let request: GroupRequest = decode_request(params, "notifications.clearGroup")?;
        if request.group_key.trim().is_empty() {
            return Err(ErrorCode::Validation.reply("group_key must not be empty"));
        }
        let closed = self
            .engine()?
            .clear_group(&request.group_key)
            .await
            .map_err(operation_error)?;
        Ok(Reply::Operation(Operation::ClearGroup {
            group_key: request.group_key,
            closed,
        })
        .body())
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
            Ok(Reply::Operation(Operation::Snooze {
                id,
                until_unix_ms: request.until_unix_ms,
            })
            .body())
        } else {
            Err(ErrorCode::SnoozeFailed
                .reply("notification is unavailable or duration has elapsed"))
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
            Ok(Reply::Operation(Operation::InvokeAction {
                id,
                action_key: request.action_key,
            })
            .body())
        } else {
            Err(ErrorCode::ActionNotFound.reply("notification or action is unavailable"))
        }
    }
    async fn reply(&self, params: Value) -> Response {
        let request: ReplyRequest = decode_request(params, "notifications.reply")?;
        let id = positive_id(request.id)?;
        if self.engine()?.reply(id, &request.text).await {
            Ok(Reply::Operation(Operation::Reply { id }).body())
        } else {
            Err(not_found(id))
        }
    }
}
fn history_error(value: HistoryError) -> Value {
    error(value.code(), value.message())
}

#[cfg(test)]
mod tests {
    use super::{NotificationApi, NotificationEngine, NotificationService};
    use serde_json::json;

    #[tokio::test]
    async fn live_mutations_match_fixtures_and_stale_actions_emit_no_signals() {
        use crate::{
            activity::notifications::model::{
                ActiveNotification, IncomingNotification, NotificationSignal,
            },
            state::StateStore,
        };
        let fixtures = crate::protocol::notification_contract_fixture().await;
        for (key, case) in fixtures["replies"].as_object().unwrap() {
            let engine = NotificationEngine::new(StateStore::default()).await;
            let record: ActiveNotification = serde_json::from_value(
                fixtures["snapshot"]["data"]["snapshot"]["notification_active"]["notifications"][0]
                    .clone(),
            )
            .unwrap();
            engine
                .notify(
                    0,
                    IncomingNotification {
                        app_name: record.app_name,
                        app_icon: record.app_icon,
                        summary: record.summary,
                        body: record.body,
                        actions: record.actions,
                        hints: record.hints,
                        expire_timeout: 0,
                    },
                )
                .await
                .unwrap();
            let api = NotificationApi::new(NotificationService::native(engine.clone()));
            let mut signals = engine.subscribe_signals();
            let method = case["method"].as_str().unwrap();
            let mut params = case["params"].clone();
            if ["delete", "cancelDelete"].contains(&key.as_str()) {
                let prepared = api.dispatch("notifications.prepareDelete", json!({})).await;
                params["token"] = prepared["data"]["delete_confirmation"]["token"].clone();
            }
            let response = api.dispatch(method, params.clone()).await;
            if key == "prepareDelete" {
                let challenge = &response["data"]["delete_confirmation"];
                assert_eq!(challenge["count"], 1);
                assert!(
                    challenge["token"]
                        .as_str()
                        .is_some_and(|token| !token.is_empty())
                );
                assert!(challenge["app_key"].is_null() && challenge["selected"].is_null());
                assert!(challenge["expires_unix_ms"].as_u64().unwrap() > 0);
            } else {
                assert_eq!(response, case["success"], "{key}");
            }
            if key == "invokeAction" {
                assert_eq!(
                    signals.try_recv().unwrap(),
                    NotificationSignal::ActionInvoked {
                        id: 1,
                        action_key: "open".into()
                    }
                );
                assert_eq!(
                    signals.try_recv().unwrap(),
                    NotificationSignal::Closed { id: 1, reason: 2 }
                );
            } else if key == "reply" {
                assert_eq!(
                    signals.try_recv().unwrap(),
                    NotificationSignal::Replied {
                        id: 1,
                        text: "Hello".into()
                    }
                );
            }
            if ["invokeAction", "reply"].contains(&key.as_str()) {
                assert!(
                    signals.try_recv().is_err(),
                    "one request must not emit duplicate application signals"
                );
            }
            if !case["rejected"].is_null() {
                assert_eq!(
                    api.dispatch(method, serde_json::Value::Null).await,
                    case["rejected"],
                    "{key} schema"
                );
            }
            if !case["stale"].is_null() {
                engine.dismiss(1).await.unwrap();
                while signals.try_recv().is_ok() {}
                assert_eq!(
                    api.dispatch(method, params).await,
                    case["stale"],
                    "{key} stale id"
                );
                assert!(
                    signals.try_recv().is_err(),
                    "stale IDs must not emit an application action/reply"
                );
            }
        }
    }

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
