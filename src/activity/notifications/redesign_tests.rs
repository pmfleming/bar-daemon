use super::{
    center::{CenterPage, CenterQuery},
    engine::NotificationEngine,
    model::{IncomingNotification, NotificationAction},
    policy::AppPolicy,
};
use crate::state::StateStore;

fn incoming(app: &str, text: &str) -> IncomingNotification {
    let mut n = super::model::incoming(app, text, "Body");
    n.hints.desktop_entry = app.into();
    n
}
fn query(value: serde_json::Value) -> CenterQuery {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn timeline_counts_complete_dates_and_bounds_exact_repeat_stacks() {
    let (_dir, _path, engine) = NotificationEngine::persistent_fixture().await;
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
    assert_eq!(challenge.count, 2);
    let arrival = engine
        .notify(0, incoming("chat", "After confirmation"))
        .await
        .unwrap();
    let token = challenge.token;
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
    let record = restarted
        .active()
        .await
        .into_iter()
        .find(|n| n.id == after_restart)
        .unwrap();
    let position = super::history::Position {
        id: record.id,
        created: record.created_unix_ms,
    };
    assert!(
        restarted
            .prepare_delete(
                None,
                Some(super::history::Position {
                    created: position.created + 1,
                    ..position
                })
            )
            .await
            .is_err()
    );
    let selected = restarted
        .prepare_delete(None, Some(position))
        .await
        .unwrap();
    assert_eq!(selected.count, 1);
    restarted.cancel_delete(&selected.token).await;
    assert!(restarted.delete_confirmed(selected.token).await.is_err());
    let selected = restarted
        .prepare_delete(None, Some(position))
        .await
        .unwrap();
    assert_eq!(restarted.delete_confirmed(selected.token).await.unwrap(), 1);
    assert_eq!(restarted.history(None, 100).await.unwrap().len(), 2);
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
