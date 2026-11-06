//! Offline query examples execute native normalization, projection and pagination.
use super::{
    query_wire::{self, HistoryRequest},
    read_wire,
};
use crate::{
    activity::notifications::{
        center::{self, CenterQuery},
        history::{self, HistoryError, HistoryQuery},
        model::*,
    },
    state::StateStore,
};
use serde_json::{Map, Value, json};
const EPOCH: &str = "contract-epoch";
const REVISION: u64 = 7;
const NOW: u64 = 1_700_001_000_000;
fn envelope(result: Result<Value, Value>) -> Value {
    result.map_or_else(std::convert::identity, super::success)
}

pub(super) async fn fixture() -> Value {
    let records: Vec<_> = (1..=76)
        .map(|id| {
            let mut record = ActiveNotification::from_incoming(
                id,
                IncomingNotification {
                    app_name: if id <= 25 {
                        "Contract Chat".into()
                    } else {
                        format!("Contract App {id}")
                    },
                    app_icon: String::new(),
                    summary: if id <= 2 {
                        "Repeated".into()
                    } else {
                        format!("Message {id}")
                    },
                    body: "Example body".into(),
                    actions: Vec::new(),
                    hints: NotificationHints::default(),
                    expire_timeout: 0,
                },
                1_700_000_000_000 + u64::from(id) * 1000,
            );
            record.identity_icon.clear();
            record
        })
        .collect();
    let app_key = center::Preview::from_active(&records[0], "").app_key;
    let store = StateStore::default();
    store
        .update_notifications(NotificationState {
            available: true,
            backend: "native".into(),
            count: 76,
            history_revision: REVISION,
            ..Default::default()
        })
        .await;
    store
        .update_notification_active(NotificationActiveState {
            available: true,
            revision: REVISION,
            notifications: records.clone(),
            toast_decks: vec![],
            error: None,
        })
        .await;
    let snapshot = read_wire::snapshot(&store.snapshot().await);
    let mut cases = Map::new();
    for (key, params) in [
        (
            "apps",
            json!({"view":"apps", "query":"", "offset":0, "epoch":null, "revision":null, "app_anchor":null}),
        ),
        (
            "apps-next",
            json!({"view":"apps", "query":"", "offset":50, "epoch":EPOCH, "revision":REVISION.to_string(), "app_anchor":null}),
        ),
        (
            "app",
            json!({"view":"app", "query":"", "app_key":app_key, "page":1, "selected":{"id":25,"created":1_700_000_025_000u64}}),
        ),
        (
            "app-next",
            json!({"view":"app", "query":"", "app_key":app_key, "page":2}),
        ),
        (
            "resolve",
            json!({"view":"app", "query":"", "group_key":records[0].group_key}),
        ),
        (
            "apps-anchor",
            json!({"view":"apps", "query":"", "offset":0, "epoch":null, "revision":null, "app_anchor":app_key}),
        ),
        (
            "apps-anchor-next",
            json!({"view":"apps", "query":"", "offset":50, "epoch":EPOCH, "revision":REVISION.to_string(), "app_anchor":app_key}),
        ),
        ("search", json!({"view":"apps", "query":"  REPEATED  "})),
        ("empty", json!({"view":"apps", "query":"no-match"})),
        (
            "timeline",
            json!({"view":"timeline", "app_key":app_key, "query":"", "date":"older", "period_groups":true, "period_day":null, "group_similar":true, "offset":0, "epoch":null, "revision":null, "timeline_anchor":null}),
        ),
        (
            "timeline-next",
            json!({"view":"timeline", "app_key":app_key, "query":"", "date":"older", "period_groups":true, "period_day":"2026-01-15", "group_similar":true, "offset":20, "epoch":EPOCH, "revision":REVISION.to_string(), "timeline_anchor":null}),
        ),
    ] {
        let mut query: CenterQuery =
            super::decode_request(params.clone(), "notifications.queryCenter").unwrap();
        query.normalize().unwrap();
        let page = center::project_at(
            records
                .iter()
                .map(|n| center::Preview::from_active(n, &query.query))
                .collect(),
            &query,
            EPOCH,
            REVISION,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            |p| {
                Ok(records
                    .iter()
                    .find(|n| n.id == p.id)
                    .unwrap()
                    .clone()
                    .into())
            },
        );
        cases.insert(key.into(), json!({"method":"notifications.queryCenter", "params":params, "success":envelope(query_wire::center(page))}));
    }
    let mut cursor = None;
    for key in ["history", "history-next"] {
        let params = json!({"query":"chat", "cursor":cursor, "anchor":null, "limit":2});
        let mut query: HistoryQuery =
            super::decode_request(params.clone(), "notifications.queryHistory").unwrap();
        query.normalize().unwrap();
        let before = query.position(EPOCH, REVISION, NOW).unwrap();
        let page = history::page(
            Vec::new(),
            records.clone(),
            &query,
            before,
            EPOCH,
            REVISION,
            NOW,
        )
        .unwrap();
        cursor.clone_from(&page.next_cursor);
        cases.insert(key.into(), json!({"method":"notifications.queryHistory", "params":params, "success":envelope(query_wire::page(Ok(page)))}));
    }
    let legacy = vec![HistoryNotification {
        history_id: 1,
        notification: records[0].clone(),
        closed_unix_ms: Some(NOW),
        close_reason: Some(2),
    }];
    cases.insert("list".into(), json!({"method":"notifications.list", "params":HistoryRequest { before_history_id:None, limit:50 }, "success":envelope(query_wire::list(Ok(legacy)))}));
    let invalid: CenterQuery = serde_json::from_value(json!({"view":"unknown"})).unwrap();
    let mut invalid = invalid;
    let stale: CenterQuery =
        serde_json::from_value(json!({"view":"apps", "offset":50, "epoch":"old", "revision":"0"}))
            .unwrap();
    json!({"snapshot":snapshot, "cases":cases, "errors":{
        "invalid":query_wire::history_error(invalid.normalize().unwrap_err()),
        "stale":query_wire::history_error(stale.validate_revision(EPOCH, REVISION).unwrap_err()),
        "busy":query_wire::history_error(HistoryError::Busy),
        "unavailable":query_wire::history_error(HistoryError::Unavailable(anyhow::anyhow!("History read unavailable"))),
        "centerRejected":super::decode_request::<CenterQuery>(Value::Null, "notifications.queryCenter").err().unwrap(),
        "historyRejected":super::decode_request::<HistoryQuery>(Value::Null, "notifications.queryHistory").err().unwrap(),
        "listRejected":super::decode_request::<HistoryRequest>(Value::Null, "notifications.list").err().unwrap(),
        "listInvalid":HistoryRequest { before_history_id:Some(0), limit:50 }.bounded_limit().unwrap_err(),
        "listFailed":query_wire::list(Err(anyhow::anyhow!("History read unavailable"))).unwrap_err()
    }})
}
