//! Activity/todo v1 boundary. Refresh acceptance is not provider completion.
use crate::{
    activity::model::{ActivityRange, ActivityState, TodoItem},
    api,
};
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
impl Reply<'_> {
    fn envelope(self) -> Value {
        api::success(serde_json::to_value(self).expect("serializable activity reply"))
    }
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
        api::error(
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
        Ok(activity_range) => Reply::Range {
            activity_range: &activity_range,
        }
        .envelope(),
        Err(error) => Failure::Query.reply(error),
    }
}
pub(crate) fn refreshed(activity: ActivityState) -> Value {
    Reply::Refresh {
        activity: &activity,
        operation: RefreshOperation {
            state: RefreshState::Accepted,
        },
    }
    .envelope()
}
pub(crate) fn todo(result: anyhow::Result<TodoItem>, failure: Failure) -> Value {
    match result {
        Ok(todo) => Reply::Todo { todo: &todo }.envelope(),
        Err(error) => failure.reply(error),
    }
}
pub(crate) fn deleted(id: String, result: anyhow::Result<()>) -> Value {
    match result {
        Ok(()) => Reply::Deleted { deleted: &id }.envelope(),
        Err(error) => Failure::Delete.reply(error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn preserves_legacy_defaults_envelopes_and_top_level_error_messages() {
        let request: CompleteTodoRequest =
            serde_json::from_value(json!({"id":"local-1", "extra":true})).unwrap();
        assert!(request.completed);
        let request: CreateTodoRequest = serde_json::from_value(json!({"title":"Task"})).unwrap();
        assert_eq!(request.priority, 0);
        assert!(request.due_date.is_none() && request.due_unix_ms.is_none());
        let state = ActivityState::default();
        assert_eq!(
            refreshed(state.clone()),
            api::success(json!({"activity":state, "operation":{"state":"accepted"}}))
        );
        assert_eq!(
            deleted("local-1".into(), Ok(())),
            api::success(json!({"deleted":"local-1"}))
        );
        assert_eq!(
            Failure::Create.reply(anyhow::anyhow!("cause").context("context")),
            api::error("todo-create-failed", "context")
        );
    }
}
