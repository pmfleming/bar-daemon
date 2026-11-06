use super::super::{
    context_actions::ContextAction,
    database::Fault,
    model::{NotificationAction, NotificationHints},
};
use super::*;
use futures::executor::block_on;

fn rich_notification(id: u32) -> ActiveNotification {
    let mut n = ActiveNotification::from_incoming(
        id,
        super::super::model::incoming("Éclair", "Code 123456", "Unicode 🦀\n%_ ' \""),
        crate::time::unix_ms(),
    );
    n.app_icon = "app-icon".into();
    n.identity_icon = "captured-icon".into();
    n.hints = NotificationHints {
        urgency: 2,
        category: "im.received".into(),
        desktop_entry: "org.example.Chat".into(),
        image_path: "/tmp/image".into(),
        sound_name: "message".into(),
        sound_file: "/tmp/sound".into(),
        resident: true,
        transient: false,
        suppress_sound: true,
        image_data_present: true,
    };
    n.actions = vec![
        NotificationAction {
            key: "z".into(),
            label: "Last alphabetically".into(),
        },
        NotificationAction {
            key: "a".into(),
            label: "First alphabetically".into(),
        },
    ];
    n.context_actions = vec![
        ContextAction {
            key: "copy-code".into(),
            label: "Copy".into(),
            icon: "copy".into(),
        },
        ContextAction {
            key: "open-link".into(),
            label: "Open".into(),
            icon: "link".into(),
        },
    ];
    n.updated_unix_ms += 2;
    n.expires_unix_ms = Some(n.created_unix_ms + 1000);
    n.snoozed_until_unix_ms = Some(n.created_unix_ms + 500);
    n.group_key = "a\0b".into();
    n.source_monitor = "DP-1".into();
    n.dnd_bypass = true;
    n.interaction_blocked = true;
    n.toast_visible = true;
    n.toast_expires_unix_ms = Some(n.created_unix_ms + 100);
    n
}
fn historical(mut n: ActiveNotification) -> ActiveNotification {
    n.interaction_blocked = false;
    n.toast_visible = false;
    n.toast_expires_unix_ms = None;
    n
}
async fn child_counts(db: &NotificationStore) -> (i64, i64, i64) {
    (
        db.integer("SELECT count(*) FROM notification_actions")
            .await
            .unwrap(),
        db.integer("SELECT count(*) FROM notification_context_actions")
            .await
            .unwrap(),
        db.integer("SELECT count(*) FROM notification_previews")
            .await
            .unwrap(),
    )
}

#[test]
fn typed_content_actions_and_preferences_round_trip_after_reopen() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut db = NotificationStore::open(&path).await.unwrap();
        let n = rich_notification(1);
        db.apply(Mutation::Save(Box::new(n.clone()))).await.unwrap();
        let settings = Settings {
            countdown: false,
            code_expiry_seconds: 300,
            history_days: 7,
        };
        let policy = AppPolicy {
            silent: true,
            until_unix_ms: Some(n.created_unix_ms + 1000),
            group_similar: false,
            bypass_dnd: true,
        };
        db.settings(Some(settings.clone())).await.unwrap();
        db.set_policy("app:'%_🦀", &policy).await.unwrap();
        db.apply(Mutation::SetDnd {
            enabled: true,
            until_unix_ms: Some(n.created_unix_ms + 2000),
        })
        .await
        .unwrap();
        db.apply(Mutation::Close {
            id: 1,
            closed_unix_ms: n.updated_unix_ms + 5,
            reason: 3,
        })
        .await
        .unwrap();
        db.checkpoint().await.unwrap();
        drop(db);
        let mut db = NotificationStore::open(&path).await.unwrap();
        let record = db.list(None, 10).await.unwrap().remove(0);
        assert_eq!(record.notification, historical(n.clone()));
        assert_eq!(record.close_reason, Some(3));
        assert_eq!(record.closed_unix_ms, Some(n.updated_unix_ms + 5));
        assert_eq!(db.settings(None).await.unwrap(), settings);
        assert_eq!(db.policies().await.unwrap()["app:'%_🦀"], policy);
        assert_eq!(
            db.load_dnd().await.unwrap(),
            (true, Some(n.created_unix_ms + 2000))
        );
        assert_eq!(db.last_id().await.unwrap(), 1);
        db.apply(Mutation::SetDnd {
            enabled: false,
            until_unix_ms: Some(123),
        })
        .await
        .unwrap();
        assert_eq!(db.load_dnd().await.unwrap(), (false, None));
        assert!(
            db.apply(Mutation::SetDnd {
                enabled: true,
                until_unix_ms: Some(u64::MAX)
            })
            .await
            .is_err()
        );
        assert!(db.connection.is_autocommit().unwrap());
        assert_eq!(db.load_dnd().await.unwrap(), (false, None));
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn replacement_rollback_transient_and_deletion_own_all_child_rows() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        let n = rich_notification(1);
        db.apply(Mutation::Save(Box::new(n.clone()))).await.unwrap();
        let id = db.list(None, 10).await.unwrap()[0].history_id;
        let mut replaced = n.clone();
        replaced.summary = "Replacement".into();
        replaced.actions.truncate(1);
        replaced.context_actions.clear();
        db.fault = Some(Fault::Children);
        assert!(
            db.apply(Mutation::Save(Box::new(replaced.clone())))
                .await
                .is_err()
        );
        assert_eq!(
            db.list(None, 10).await.unwrap()[0].notification,
            historical(n)
        );
        assert_eq!(child_counts(&db).await, (2, 2, 1));
        db.apply(Mutation::Save(Box::new(replaced.clone())))
            .await
            .unwrap();
        assert_eq!(db.list(None, 10).await.unwrap()[0].history_id, id);
        assert_eq!(child_counts(&db).await, (1, 0, 1));
        let targets = db.targets(None, None).await.unwrap();
        db.fault = Some(Fault::DeleteChildren);
        assert!(db.delete(&targets).await.is_err());
        assert_eq!(child_counts(&db).await, (1, 0, 1));
        assert_eq!(
            db.list(None, 10).await.unwrap()[0].notification,
            historical(replaced.clone())
        );
        replaced.hints.transient = true;
        db.apply(Mutation::Save(Box::new(replaced))).await.unwrap();
        assert!(db.list(None, 10).await.unwrap().is_empty());
        assert_eq!(child_counts(&db).await, (0, 0, 0));
        db.apply(Mutation::Save(Box::new(rich_notification(2))))
            .await
            .unwrap();
        db.delete(&db.targets(None, None).await.unwrap())
            .await
            .unwrap();
        assert_eq!(child_counts(&db).await, (0, 0, 0));
        let mut expired = rich_notification(3);
        expired.created_unix_ms = 1;
        db.apply(Mutation::Save(Box::new(expired))).await.unwrap();
        assert_eq!(db.prune().await.unwrap(), 1);
        assert_eq!(child_counts(&db).await, (0, 0, 0));
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn schema_constraints_and_rejection_of_v1_preserve_existing_state() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut db = NotificationStore::open(&path).await.unwrap();
        for sql in [
            "UPDATE notification_state SET dnd=2",
            "UPDATE notification_settings SET history_days=31",
            "UPDATE notification_state SET last_notification_id=-1",
        ] {
            assert!(db.connection.execute(sql, ()).await.is_err(), "{sql}");
        }
        assert_eq!(db.integer("SELECT count(*) FROM sqlite_schema WHERE sql LIKE '%payload_json%' OR sql LIKE '%preview_json%' OR name='notification_meta'").await.unwrap(), 0);
        db.apply(Mutation::Save(Box::new(rich_notification(1))))
            .await
            .unwrap();
        db.connection
            .execute("PRAGMA user_version=1", ())
            .await
            .unwrap();
        db.checkpoint().await.unwrap();
        drop(db);
        let bytes = std::fs::read(&path).unwrap();
        assert!(NotificationStore::open(&path).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    });
}

#[test]
fn sql_literal_search_and_cursor_order_match_rust_reference() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        let mut notifications = Vec::new();
        for (index, text) in [
            "ÉCLAIR %_",
            "x\0y",
            "İstanbul",
            "Quote ' OR 1=1 --",
            "🦀 hello",
            "no match",
        ]
        .into_iter()
        .enumerate()
        {
            let mut n = rich_notification(u32::try_from(index + 1).unwrap());
            n.summary = text.into();
            n.body = "body".into();
            // Different insertion and cursor order, including tied timestamps.
            n.created_unix_ms = 100 + u64::try_from(index % 3).unwrap();
            db.apply(Mutation::Save(Box::new(n.clone()))).await.unwrap();
            notifications.push(n);
        }
        let excluded = [(notifications[1].created_unix_ms, notifications[1].id)];
        for query in [
            "",
            "éclair",
            "%_",
            "_",
            "x\0y",
            "\0",
            "i\u{307}",
            "' or 1=1 --",
            "🦀",
            "absent",
        ] {
            let previews = db.candidates(query).await.unwrap();
            for n in &notifications {
                assert_eq!(
                    previews.iter().find(|p| p.id == n.id).unwrap().matches,
                    search_text(n).contains(query),
                    "{query:?}"
                );
            }
            for before in [
                None,
                Some(Position {
                    created: 102,
                    id: 3,
                }),
            ] {
                let mut expected = notifications
                    .iter()
                    .filter(|n| {
                        search_text(n).contains(query)
                            && before.is_none_or(|p| (n.created_unix_ms, n.id) < (p.created, p.id))
                            && !excluded.contains(&(n.created_unix_ms, n.id))
                    })
                    .collect::<Vec<_>>();
                expected.sort_by_key(|n| std::cmp::Reverse((n.created_unix_ms, n.id)));
                let actual = db.query(query, before, 2, &excluded).await.unwrap();
                assert_eq!(
                    actual.iter().map(|r| r.notification.id).collect::<Vec<_>>(),
                    expected.iter().take(3).map(|n| n.id).collect::<Vec<_>>(),
                    "{query:?}"
                );
                for record in actual {
                    let original = notifications
                        .iter()
                        .find(|n| n.id == record.notification.id)
                        .unwrap();
                    assert_eq!(record.notification, historical(original.clone()));
                }
            }
        }
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn full_live_overlay_and_bulk_delete_stay_within_planner_bounds() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        let mut excluded = Vec::new();
        db.begin().await.unwrap();
        for id in 1..=200 {
            let mut n = rich_notification(id);
            n.created_unix_ms = 100 + u64::from(id);
            excluded.push((n.created_unix_ms, n.id));
            db.save_inner(&n).await.unwrap();
        }
        db.finish(Ok(())).await.unwrap();
        assert!(db.query("", None, 100, &excluded).await.unwrap().is_empty());
        let page = db.query("", None, 10, &excluded[10..]).await.unwrap();
        assert_eq!(
            page.iter().map(|r| r.notification.id).collect::<Vec<_>>(),
            (1..=10).rev().collect::<Vec<_>>()
        );
        db.delete(&db.targets(None, None).await.unwrap())
            .await
            .unwrap();
        assert!(db.list(None, 100).await.unwrap().is_empty());
        assert_eq!(child_counts(&db).await, (0, 0, 0));
        db.checkpoint().await.unwrap();
    });
}
