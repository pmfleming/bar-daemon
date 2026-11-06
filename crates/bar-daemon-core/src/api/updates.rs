//! Read-only update routes and their wire error vocabulary stay together.
use crate::updates::{history, overview};
use serde_json::Value;

pub(super) async fn dispatch(method: &str, params: Value) -> Value {
    let (result, code) = match method {
        "updates.getOverview" => (overview::get(params).await, "update-overview-error"),
        "updates.readLog" => (history::read_log(params).await, "update-log-error"),
        "updates.queryHistory" => (history::query(params).await, "update-history-error"),
        _ => return super::unsupported_method(method),
    };
    reply(result, code)
}

fn reply(result: anyhow::Result<Value>, code: &str) -> Value {
    match result {
        Ok(data) => super::success(data),
        Err(error) => super::error(
            if code == "update-history-error" && error.is::<history::StaleCursor>() {
                "update-history-cursor-stale"
            } else {
                code
            },
            error.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{dispatch, history::StaleCursor, reply};
    use serde_json::json;

    #[tokio::test]
    async fn read_routes_preserve_error_codes_and_reject_invalid_requests_before_effects() {
        for (method, code) in [
            ("updates.getOverview", "update-overview-error"),
            ("updates.readLog", "update-log-error"),
        ] {
            let response = dispatch(method, json!({"unexpected":true})).await;
            assert_eq!(response["error"]["code"], code);
        }
        assert_eq!(
            dispatch("updates.unknown", json!({})).await,
            crate::protocol::unsupported_method("updates.unknown")
        );
        assert_eq!(
            reply(Ok(json!({"history":{}})), "unused"),
            crate::protocol::success(json!({"history":{}}))
        );
        for (error, expected) in [
            (anyhow::anyhow!("invalid"), "update-history-error"),
            (StaleCursor.into(), "update-history-cursor-stale"),
        ] {
            assert_eq!(
                reply(Err(error), "update-history-error")["error"]["code"],
                expected
            );
        }
    }
}
