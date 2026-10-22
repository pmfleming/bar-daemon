//! Offline mutation examples use live DTOs, serializers and store publication.
//! No notification server, application signal forwarding or persistence is started.
use super::{notification_wire::*, read_wire};
use crate::{
    activity::notifications::{model::*, policy::AppPolicy},
    api,
    state::StateStore,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

fn value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}
fn rejected<T: DeserializeOwned>(method: &str) -> Option<Value> {
    Some(
        api::decode_request::<T>(Value::Null, method)
            .err()
            .expect("invalid request"),
    )
}
fn failed(code: ErrorCode) -> Value {
    code.reply("Notification operation unavailable")
}

pub(super) async fn fixture() -> Value {
    let mut record = ActiveNotification::from_incoming(
        1,
        IncomingNotification {
            app_name: "Contract Chat".into(),
            app_icon: String::new(),
            summary: "A message".into(),
            body: "Example body".into(),
            actions: vec![
                NotificationAction {
                    key: "open".into(),
                    label: "Open".into(),
                },
                NotificationAction {
                    key: "inline-reply".into(),
                    label: "Reply".into(),
                },
            ],
            hints: NotificationHints::default(),
            expire_timeout: 0,
        },
        1_700_000_000_000,
    );
    record.identity_icon.clear(); // Deterministic seed, independent of installed application icons.
    let summary = NotificationState {
        available: true,
        count: 1,
        text: "1".into(),
        tooltip: "1 Notification".into(),
        alt: "notification".into(),
        class_name: "notification".into(),
        backend: "native".into(),
        history_revision: 1,
        settings: Some(Default::default()),
        sharing: Some(Default::default()),
        ..Default::default()
    };
    let active = NotificationActiveState {
        available: true,
        revision: 1,
        notifications: vec![record.clone()],
        toast_decks: vec![],
        error: None,
    };
    let store = StateStore::default();
    store.update_notifications(summary.clone()).await;
    store.update_notification_active(active.clone()).await;
    let snapshot = read_wire::snapshot(&store.snapshot().await);
    let until = 4_102_444_800_000;
    let challenge = DeleteConfirmation {
        token: "delete-example".into(),
        count: 1,
        app_key: None,
        selected: None,
        expires_unix_ms: until,
    };
    let mut dnd = summary.clone();
    dnd.dnd = true;
    dnd.alt = "dnd-notification".into();
    dnd.class_name = dnd.alt.clone();
    let policy = AppPolicy {
        silent: true,
        ..Default::default()
    };
    let app_key = "named:contract chat";
    let mut policies = summary.clone();
    policies.history_revision = 2;
    policies.app_policies.insert(app_key.into(), policy.clone());
    let mut replies = Map::new();
    for key in [
        "togglePanel",
        "toggleDnd",
        "setDnd",
        "setAppPolicy",
        "prepareDelete",
        "delete",
        "cancelDelete",
        "dismiss",
        "clear",
        "clearGroup",
        "snooze",
        "invokeAction",
        "reply",
    ] {
        let method = format!(
            "notifications.{}",
            if key == "cancelDelete" { "delete" } else { key }
        );
        let (params, body, error, rejected) = match key {
            "togglePanel" => (
                json!({}),
                Reply::Operation(Operation::TogglePanel),
                failed(ErrorCode::OperationFailed),
                None,
            ),
            "toggleDnd" => (
                json!({}),
                Reply::Operation(Operation::ToggleDnd { enabled: true }),
                failed(ErrorCode::OperationFailed),
                None,
            ),
            "setDnd" => (
                value(DndRequest {
                    enabled: true,
                    until_unix_ms: None,
                }),
                Reply::Notifications {
                    notifications: &dnd,
                },
                failed(ErrorCode::OperationFailed),
                rejected::<DndRequest>(&method),
            ),
            "setAppPolicy" => (
                value(PolicyRequest {
                    app_key: app_key.into(),
                    policy: policy.clone(),
                }),
                Reply::Notifications {
                    notifications: &policies,
                },
                failed(ErrorCode::PolicyFailed),
                rejected::<PolicyRequest>(&method),
            ),
            "prepareDelete" => (
                value(DeleteScope {
                    app_key: None,
                    selected: None,
                }),
                Reply::DeleteConfirmation {
                    delete_confirmation: &challenge,
                },
                failed(ErrorCode::DeleteFailed),
                rejected::<DeleteScope>(&method),
            ),
            "delete" => (
                value(DeleteRequest {
                    token: challenge.token.clone(),
                    cancel: false,
                }),
                Reply::Deleted { deleted: 1 },
                delete_error(anyhow::anyhow!(
                    "Delete confirmation expired; request a new confirmation"
                )),
                rejected::<DeleteRequest>(&method),
            ),
            "cancelDelete" => (
                value(DeleteRequest {
                    token: challenge.token.clone(),
                    cancel: true,
                }),
                Reply::Cancelled { cancelled: true },
                ErrorCode::NativeRequired.reply("native notifications are disabled"),
                rejected::<DeleteRequest>(&method),
            ),
            "dismiss" => (
                value(NotificationRequest { id: 1 }),
                Reply::Operation(Operation::Dismiss { id: 1 }),
                failed(ErrorCode::OperationFailed),
                rejected::<NotificationRequest>(&method),
            ),
            "clear" => (
                json!({}),
                Reply::Operation(Operation::Clear { closed: 1 }),
                failed(ErrorCode::OperationFailed),
                None,
            ),
            "clearGroup" => (
                value(GroupRequest {
                    group_key: record.group_key.clone(),
                }),
                Reply::Operation(Operation::ClearGroup {
                    group_key: record.group_key.clone(),
                    closed: 1,
                }),
                failed(ErrorCode::OperationFailed),
                rejected::<GroupRequest>(&method),
            ),
            "snooze" => (
                value(SnoozeRequest {
                    id: 1,
                    until_unix_ms: until,
                }),
                Reply::Operation(Operation::Snooze {
                    id: 1,
                    until_unix_ms: until,
                }),
                failed(ErrorCode::OperationFailed),
                rejected::<SnoozeRequest>(&method),
            ),
            "invokeAction" => (
                value(ActionRequest {
                    id: 1,
                    action_key: "open".into(),
                    activation_token: None,
                }),
                Reply::Operation(Operation::InvokeAction {
                    id: 1,
                    action_key: "open".into(),
                }),
                failed(ErrorCode::OperationFailed),
                rejected::<ActionRequest>(&method),
            ),
            "reply" => (
                value(ReplyRequest {
                    id: 1,
                    text: "Hello".into(),
                }),
                Reply::Operation(Operation::Reply { id: 1 }),
                not_found(1),
                rejected::<ReplyRequest>(&method),
            ),
            _ => unreachable!(),
        };
        let stale = match key {
            "dismiss" | "reply" => Some(not_found(1)),
            "snooze" => Some(
                ErrorCode::SnoozeFailed
                    .reply("notification is unavailable or duration has elapsed"),
            ),
            "invokeAction" => {
                Some(ErrorCode::ActionNotFound.reply("notification or action is unavailable"))
            }
            _ => None,
        };
        replies.insert(key.into(), json!({"method": method, "params": params, "success": api::success(body.body()), "failed": error, "rejected": rejected, "stale": stale}));
    }
    let mut changes = store.subscribe();
    store.update_notifications(dnd).await;
    let dnd_event =
        read_wire::Event::changed("subscription-example", changes.try_recv().unwrap()).into_value();
    store.update_notifications(summary).await;
    let dnd_cleared_event =
        read_wire::Event::changed("subscription-example", changes.try_recv().unwrap()).into_value();
    let mut replaced = active;
    replaced.revision = 2;
    replaced.notifications[0].created_unix_ms += 1000;
    replaced.notifications[0].updated_unix_ms += 1000;
    store.update_notification_active(replaced).await;
    let replaced_event =
        read_wire::Event::changed("subscription-example", changes.try_recv().unwrap()).into_value();
    store
        .update_notification_active(NotificationActiveState {
            available: true,
            revision: 3,
            notifications: vec![],
            toast_decks: vec![],
            error: None,
        })
        .await;
    let closed_event =
        read_wire::Event::changed("subscription-example", changes.try_recv().unwrap()).into_value();
    json!({"snapshot": snapshot, "replies": replies, "events": {"dnd": dnd_event, "dndCleared": dnd_cleared_event, "replaced": replaced_event, "closed": closed_event}})
}
