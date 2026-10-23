//! Notification read bodies. NotificationApi owns the single outer envelope.
use super::notification_wire::ErrorCode;
use crate::activity::notifications::{
    center::CenterPage,
    history::{HistoryError, HistoryPage},
    model::HistoryNotification,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
pub(crate) struct HistoryRequest {
    pub before_history_id: Option<i64>,
    #[serde(default = "default_history_limit")]
    pub limit: usize,
}
const fn default_history_limit() -> usize {
    50
}
impl HistoryRequest {
    pub(crate) fn bounded_limit(&self) -> Result<usize, Value> {
        if self.before_history_id.is_some_and(|id| id <= 0) {
            return Err(ErrorCode::Validation.reply("before_history_id must be positive or null"));
        }
        Ok(self.limit.min(200))
    }
}
#[derive(Serialize)]
#[serde(untagged)]
enum Reply {
    Center {
        notification_center: CenterPage,
    },
    Page {
        notification_page: HistoryPage,
    },
    List {
        notification_history: Vec<HistoryNotification>,
    },
}
impl Reply {
    fn body(self) -> Value {
        serde_json::to_value(self).expect("serializable notification query reply")
    }
}
pub(crate) fn history_error(error: HistoryError) -> Value {
    super::error(error.code(), error.message())
}
pub(crate) fn center(result: Result<CenterPage, HistoryError>) -> Result<Value, Value> {
    result
        .map(|notification_center| {
            Reply::Center {
                notification_center,
            }
            .body()
        })
        .map_err(history_error)
}
pub(crate) fn page(result: Result<HistoryPage, HistoryError>) -> Result<Value, Value> {
    result
        .map(|notification_page| Reply::Page { notification_page }.body())
        .map_err(history_error)
}
pub(crate) fn list(result: anyhow::Result<Vec<HistoryNotification>>) -> Result<Value, Value> {
    result
        .map(|notification_history| {
            Reply::List {
                notification_history,
            }
            .body()
        })
        .map_err(|e| super::error("notification-history-failed", e.to_string()))
}
