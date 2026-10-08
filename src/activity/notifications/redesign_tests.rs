use super::{
    center::{CenterPage, CenterQuery},
    engine::NotificationEngine,
    model::{IncomingNotification, NotificationAction, NotificationHints},
    policy::AppPolicy,
};
use crate::state::StateStore;

fn incoming(app: &str, text: &str) -> IncomingNotification {
    IncomingNotification {
        app_name: app.into(),
        app_icon: String::new(),
        summary: text.into(),
        body: "Body".into(),
        actions: vec![],
        hints: NotificationHints {
            desktop_entry: app.into(),
            ..Default::default()
        },
        expire_timeout: 0,
    }
}
fn query(value: serde_json::Value) -> CenterQuery {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn timeline_counts_complete_dates_and_bounds_exact_repeat_stacks() {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        NotificationEngine::persistent(StateStore::default(), dir.path().join("notifications.db"))
            .await
            .unwrap();
    for _ in 0..65 {
        let id = engine
            .notify(0, incoming("chat", "Repeated event"))
            .await
            .unwrap();
        engine.dismiss(id).await.unwrap();
    }
    let mut actionable = incoming("chat", "Repeated event");
    actionable.actions.push(NotificationAction {
        key: "default".into(),
        label: "Open".into(),
    });
    engine.notify(0, actionable).await.unwrap();
    let mut request = query(serde_json::json!({"view":"timeline", "app_key":"desktop:chat"}));
    let CenterPage::Timeline(page) = engine.query_center(request.clone()).await.unwrap() else {
        panic!()
    };
    assert_eq!(page.count, 66);
    assert_eq!(page.dates.iter().map(|d| d.count).sum::<usize>(), 66);
    assert_eq!(page.total_rows, 3);
    assert_eq!(
        page.entries.iter().map(|e| e.members.len()).sum::<usize>(),
        66
    );
    assert_eq!(
        page.entries[0].members.len(),
        1,
        "sender actions never stack into a hidden bulk invocation"
    );
    assert!(page.entries.iter().all(|entry| entry.members.len() <= 50));
    request.group_similar = false;
    let CenterPage::Timeline(page) = engine.query_center(request.clone()).await.unwrap() else {
        panic!()
    };
    assert_eq!(page.entries.len(), 20);
    assert_eq!(page.total_rows, 66);
    request.offset = page.next_offset.unwrap();
    request.epoch = Some(page.epoch);
    request.revision = Some(page.revision);
    let CenterPage::Timeline(next) = engine.query_center(request.clone()).await.unwrap() else {
        panic!()
    };
    assert_eq!(next.entries.len(), 20);
    engine.notify(0, incoming("chat", "Arrival")).await.unwrap();
    assert!(matches!(
        engine.query_center(request).await,
        Err(super::history::HistoryError::Stale)
    ));
}

#[tokio::test]
async fn confirmed_delete_removes_saved_and_active_records_but_not_arrivals_or_app_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notifications.db");
    let state = StateStore::default();
    let engine = NotificationEngine::persistent(state.clone(), path.clone())
        .await
        .unwrap();
    let policy = AppPolicy {
        silent: true,
        ..Default::default()
    };
    engine
        .set_app_policy("desktop:chat".into(), policy.clone())
        .await
        .unwrap();
    let archived = engine
        .notify(0, incoming("chat", "Archived"))
        .await
        .unwrap();
    engine.dismiss(archived).await.unwrap();
    let active = engine.notify(0, incoming("chat", "Active")).await.unwrap();
    engine
        .notify(0, incoming("other", "Unrelated"))
        .await
        .unwrap();
    let challenge = engine
        .prepare_delete(Some("desktop:chat".into()), None)
        .await
        .unwrap();
    assert_eq!(challenge["count"], 2);
    let arrival = engine
        .notify(0, incoming("chat", "After confirmation"))
        .await
        .unwrap();
    let token = challenge["token"].as_str().unwrap().to_owned();
    assert_eq!(engine.delete_confirmed(token.clone()).await.unwrap(), 2);
    assert!(
        engine.delete_confirmed(token).await.is_err(),
        "confirmation cannot replay"
    );
    let active_rows = engine.active().await;
    assert!(!active_rows.iter().any(|n| n.id == active));
    assert!(
        active_rows
            .iter()
            .any(|n| n.id == arrival && !n.toast_visible)
    );
    let stored = engine.history(None, 100).await.unwrap();
    assert_eq!(stored.len(), 2);
    assert!(
        !stored
            .iter()
            .any(|r| r.notification.id == archived || r.notification.id == active)
    );
    let restarted = NotificationEngine::persistent(StateStore::default(), path)
        .await
        .unwrap();
    assert_eq!(restarted.history(None, 100).await.unwrap().len(), 2);
    let CenterPage::Apps { apps, .. } = restarted
        .query_center(query(serde_json::json!({"view":"apps"})))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        apps.iter().find(|a| a.key == "desktop:chat").unwrap().count,
        1
    );
    let summary = state.read(|s| s.notifications.clone()).await;
    assert_eq!(summary.app_policies["desktop:chat"], policy);
    let after_restart = restarted
        .notify(0, incoming("chat", "Silent after restart"))
        .await
        .unwrap();
    assert!(
        !restarted
            .active()
            .await
            .iter()
            .find(|n| n.id == after_restart)
            .unwrap()
            .toast_visible
    );
}

#[tokio::test]
async fn cancelled_confirmation_and_single_record_identity_are_safe() {
    let engine = NotificationEngine::new(StateStore::default()).await;
    let id = engine.notify(0, incoming("chat", "Message")).await.unwrap();
    let challenge = engine.prepare_delete(None, None).await.unwrap();
    let token = challenge["token"].as_str().unwrap();
    engine.cancel_delete(token).await;
    assert!(engine.delete_confirmed(token.into()).await.is_err());
    assert_eq!(engine.active().await.len(), 1);
    let record = engine.active().await.remove(0);
    assert!(
        engine
            .prepare_delete(
                None,
                Some(super::history::Position {
                    id,
                    created: record.created_unix_ms + 1
                })
            )
            .await
            .is_err()
    );
    let challenge = engine
        .prepare_delete(
            None,
            Some(super::history::Position {
                id,
                created: record.created_unix_ms,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        engine
            .delete_confirmed(challenge["token"].as_str().unwrap().into())
            .await
            .unwrap(),
        1
    );
    assert!(engine.active().await.is_empty());
}

#[tokio::test]
async fn delete_includes_retained_records_older_than_the_search_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notifications.db");
    let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    {
        let mut db = rusqlite::Connection::open(&path).unwrap();
        let transaction = db.transaction().unwrap();
        for id in 1..=5005 {
            let record = super::model::ActiveNotification::from_incoming(
                id,
                incoming("chat", "Old history"),
                u64::from(id) * 1000,
            );
            transaction.execute("INSERT INTO notifications(session_id,payload_json,search_text,created_unix_ms,updated_unix_ms,closed_unix_ms) VALUES (?1,?2,'chat',?3,?3,?3)", rusqlite::params![id, serde_json::to_string(&record).unwrap(), record.created_unix_ms]).unwrap();
        }
        transaction.commit().unwrap();
    }
    let challenge = engine
        .prepare_delete(Some("desktop:chat".into()), None)
        .await
        .unwrap();
    assert_eq!(challenge["count"], 5005);
    engine
        .delete_confirmed(challenge["token"].as_str().unwrap().into())
        .await
        .unwrap();
    assert!(
        engine.history(None, 100).await.unwrap().is_empty(),
        "older records must not reappear after deleting the visible search window"
    );
}

#[tokio::test]
async fn app_policy_applies_before_popup_publication_and_is_app_scoped() {
    let state = StateStore::default();
    let engine = NotificationEngine::new(state.clone()).await;
    engine
        .notify(0, incoming("quiet", "Already showing"))
        .await
        .unwrap();
    engine
        .set_app_policy(
            "desktop:quiet".into(),
            AppPolicy {
                silent: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    engine
        .set_app_policy(
            "desktop:urgent".into(),
            AppPolicy {
                bypass_dnd: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    engine.set_dnd(true, None).await.unwrap();
    engine.notify(0, incoming("quiet", "Silent")).await.unwrap();
    engine
        .notify(0, incoming("urgent", "Allowed"))
        .await
        .unwrap();
    let active = state.read(|s| s.notification_active.clone()).await;
    assert!(
        !active
            .notifications
            .iter()
            .find(|n| n.app_name == "quiet")
            .unwrap()
            .toast_visible
    );
    assert!(
        active
            .notifications
            .iter()
            .find(|n| n.app_name == "urgent")
            .unwrap()
            .dnd_bypass
    );
    engine.set_dnd(false, None).await.unwrap();
    engine
        .set_app_policy("desktop:quiet".into(), AppPolicy::default())
        .await
        .unwrap();
    assert!(
        state
            .read(|s| s
                .notification_active
                .notifications
                .iter()
                .filter(|n| n.app_name == "quiet")
                .all(|n| !n.toast_visible))
            .await,
        "unsilencing must not replay old popups"
    );
    assert!(
        engine
            .set_app_policy(
                "desktop:quiet".into(),
                AppPolicy {
                    silent: true,
                    until_unix_ms: Some(1),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
}
