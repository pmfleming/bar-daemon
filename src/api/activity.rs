use std::sync::Arc;

use super::{error, success};
use crate::{activity::ActivityService, state::StateStore};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct RangeRequest {
    from_unix_ms: i64,
    to_unix_ms: i64,
}

#[derive(Deserialize)]
struct CreateTodoRequest {
    title: String,
    due_unix_ms: Option<i64>,
    due_date: Option<String>,
    #[serde(default)]
    priority: u8,
}

#[derive(Deserialize)]
struct CompleteTodoRequest {
    id: String,
    #[serde(default = "default_true")]
    completed: bool,
}

#[derive(Deserialize)]
struct TodoRequest {
    id: String,
}

const fn default_true() -> bool {
    true
}

#[derive(Clone)]
pub(super) struct ActivityApi {
    state: StateStore,
    activity: Arc<ActivityService>,
}

impl ActivityApi {
    pub(super) fn new(state: StateStore, activity: Arc<ActivityService>) -> Self {
        Self { state, activity }
    }

    pub(super) async fn activity_query_range(&self, params: Value) -> Value {
        let request = request!(params, RangeRequest, "activity.queryRange");
        match self
            .activity
            .query_range(request.from_unix_ms, request.to_unix_ms)
            .await
        {
            Ok(range) => success(json!({"activity_range": range})),
            Err(value) => error("activity-query-failed", value.to_string()),
        }
    }
    pub(super) async fn activity_refresh(&self) -> Value {
        self.activity.refresh().await;
        success(json!({"activity": self.state.snapshot().await.activity}))
    }
    pub(super) async fn todo_create(&self, params: Value) -> Value {
        let request = request!(params, CreateTodoRequest, "todos.create");
        match self
            .activity
            .create_todo(
                request.title,
                request.due_unix_ms,
                request.due_date,
                request.priority,
            )
            .await
        {
            Ok(todo) => success(json!({"todo": todo})),
            Err(value) => error("todo-create-failed", value.to_string()),
        }
    }
    pub(super) async fn todo_complete(&self, params: Value) -> Value {
        let request = request!(params, CompleteTodoRequest, "todos.complete");
        match self
            .activity
            .complete_todo(&request.id, request.completed)
            .await
        {
            Ok(todo) => success(json!({"todo": todo})),
            Err(value) => error("todo-complete-failed", value.to_string()),
        }
    }
    pub(super) async fn todo_delete(&self, params: Value) -> Value {
        let request = request!(params, TodoRequest, "todos.delete");
        match self.activity.delete_todo(&request.id).await {
            Ok(()) => success(json!({"deleted": request.id})),
            Err(value) => error("todo-delete-failed", value.to_string()),
        }
    }
}
