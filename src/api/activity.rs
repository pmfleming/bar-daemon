use super::{ApiService, error, success};
use serde_json::{Value, json};

impl ApiService {
    pub(super) async fn activity_query_range(&self, params: Value) -> Value {
        let Some(from) = params.get("from_unix_ms").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "activity.queryRange requires integer from_unix_ms",
            );
        };
        let Some(to) = params.get("to_unix_ms").and_then(Value::as_i64) else {
            return error(
                "validation-error",
                "activity.queryRange requires integer to_unix_ms",
            );
        };
        match self.activity.query_range(from, to).await {
            Ok(range) => success(json!({"activity_range": range})),
            Err(value) => error("activity-query-failed", value.to_string()),
        }
    }
    pub(super) async fn activity_refresh(&self) -> Value {
        self.activity.refresh().await;
        success(json!({"activity": self.state.snapshot().await.activity}))
    }
    pub(super) async fn todo_create(&self, params: Value) -> Value {
        let Some(title) = params.get("title").and_then(Value::as_str) else {
            return error("validation-error", "todos.create requires title");
        };
        let due_unix_ms = match params.get("due_unix_ms") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_i64() {
                Some(value) => Some(value),
                None => return error("validation-error", "due_unix_ms must be integer or null"),
            },
        };
        let due_date = match params.get("due_date") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => return error("validation-error", "due_date must be YYYY-MM-DD or null"),
        };
        let priority = params.get("priority").and_then(Value::as_u64).unwrap_or(0);
        let Ok(priority) = u8::try_from(priority) else {
            return error("validation-error", "todo priority is out of range");
        };
        match self
            .activity
            .create_todo(title.into(), due_unix_ms, due_date, priority)
            .await
        {
            Ok(todo) => success(json!({"todo": todo})),
            Err(value) => error("todo-create-failed", value.to_string()),
        }
    }
    pub(super) async fn todo_complete(&self, params: Value) -> Value {
        let Some(id) = params.get("id").and_then(Value::as_str) else {
            return error("validation-error", "todos.complete requires id");
        };
        let completed = params
            .get("completed")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        match self.activity.complete_todo(id, completed).await {
            Ok(todo) => success(json!({"todo": todo})),
            Err(value) => error("todo-complete-failed", value.to_string()),
        }
    }
    pub(super) async fn todo_delete(&self, params: Value) -> Value {
        let Some(id) = params.get("id").and_then(Value::as_str) else {
            return error("validation-error", "todos.delete requires id");
        };
        match self.activity.delete_todo(id).await {
            Ok(()) => success(json!({"deleted": id})),
            Err(value) => error("todo-delete-failed", value.to_string()),
        }
    }
}
