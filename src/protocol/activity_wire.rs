//! Activity/todo v1 boundary. Refresh acceptance is not provider completion.
use crate::activity::model::{ActivityRange, ActivityState, TodoItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
pub(crate) struct RangeRequest {
    pub from_unix_ms: i64,
    pub to_unix_ms: i64,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct CreateTodoRequest {
    pub title: String,
    pub due_unix_ms: Option<i64>,
    pub due_date: Option<String>,
    #[serde(default)]
    pub priority: u8,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct CompleteTodoRequest {
    pub id: String,
    #[serde(default = "default_true")]
    pub completed: bool,
}
const fn default_true() -> bool {
    true
}
#[derive(Deserialize, Serialize)]
pub(crate) struct TodoRequest {
    pub id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum RefreshState {
    Accepted,
}
#[derive(Serialize)]
struct RefreshOperation {
    state: RefreshState,
}
#[derive(Serialize)]
#[serde(untagged)]
enum Reply<'a> {
    Range {
        activity_range: &'a ActivityRange,
    },
    Refresh {
        activity: &'a ActivityState,
        operation: RefreshOperation,
    },
    Todo {
        todo: &'a TodoItem,
    },
    Deleted {
        deleted: &'a str,
    },
}
#[derive(Clone, Copy)]
pub(crate) enum Failure {
    Query,
    Create,
    Complete,
    Delete,
}
impl Failure {
    pub(crate) fn reply(self, error: anyhow::Error) -> Value {
        super::error(
            match self {
                Self::Query => "activity-query-failed",
                Self::Create => "todo-create-failed",
                Self::Complete => "todo-complete-failed",
                Self::Delete => "todo-delete-failed",
            },
            error.to_string(),
        )
    }
}
pub(crate) fn range(result: anyhow::Result<ActivityRange>) -> Value {
    match result {
        Ok(activity_range) => super::serialized_success(Reply::Range {
            activity_range: &activity_range,
        }),
        Err(error) => Failure::Query.reply(error),
    }
}
pub(crate) fn refreshed(activity: ActivityState) -> Value {
    super::serialized_success(Reply::Refresh {
        activity: &activity,
        operation: RefreshOperation {
            state: RefreshState::Accepted,
        },
    })
}
pub(crate) fn todo(result: anyhow::Result<TodoItem>, failure: Failure) -> Value {
    match result {
        Ok(todo) => super::serialized_success(Reply::Todo { todo: &todo }),
        Err(error) => failure.reply(error),
    }
}
pub(crate) fn deleted(id: String, result: anyhow::Result<()>) -> Value {
    match result {
        Ok(()) => super::serialized_success(Reply::Deleted { deleted: &id }),
        Err(error) => Failure::Delete.reply(error),
    }
}
