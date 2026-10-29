use super::*;
use futures::executor::block_on;

fn notification(id: u32) -> ActiveNotification {
    ActiveNotification::from_incoming(
        id,
        super::super::model::incoming("Chat", "ÉCLAIR 100%_", "body"),
        crate::time::unix_ms(),
    )
}
fn query(value: serde_json::Value) -> CenterQuery {
    serde_json::from_value(value).unwrap()
}

#[test]
fn persisted_preview_matches_rust_projection_and_replacement_is_atomic() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        for (summary, body, category, actions) in [
            (159, 239, 256, false),
            (160, 240, 257, false),
            (10, 10, 10, true),
        ] {
            let mut n = notification(1);
            n.summary = "é".repeat(summary);
            n.body = "🦀".repeat(body);
            n.hints.category = "x".repeat(category);
            n.group_key = "group".into();
            n.identity_icon = "captured-icon".into();
            if actions {
                n.actions.push(super::super::model::NotificationAction {
                    key: "default".into(),
                    label: "Open".into(),
                });
            }
            db.apply(Mutation::Save(Box::new(n.clone()))).await.unwrap();
            let rows = db.candidates("é").await.unwrap();
            assert_eq!(rows.len(), 1);
            let expected = Preview::from_active(&n, "é");
            assert_eq!(
                serde_json::to_value(&rows[0]).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert_eq!(rows[0].repeat_key, expected.repeat_key);
            assert_eq!(rows[0].group_key, expected.group_key);
            assert!(rows[0].matches);
            assert!(!db.candidates("absent").await.unwrap()[0].matches);
        }
        let current = db.record(1).await.unwrap();
        db.fault = Some(super::super::database::Fault::Save);
        assert!(
            db.apply(Mutation::Save(Box::new(notification(9))))
                .await
                .is_err()
        );
        assert_eq!(
            db.last_id().await.unwrap(),
            1,
            "failed save rolls back ID metadata too"
        );
        assert_eq!(db.candidates("").await.unwrap().len(), 1);
        assert_eq!(
            db.record(1).await.unwrap().notification.summary,
            current.notification.summary
        );
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn byte_bounded_pages_progress_without_duplicates() {
    block_on(async {
        use super::super::history::{self, HistoryQuery};
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        for id in 1..=12 {
            let mut n = notification(id);
            n.body = "é".repeat(30_000);
            db.apply(Mutation::Save(Box::new(n))).await.unwrap();
        }
        let now = crate::time::unix_ms();
        let mut query = HistoryQuery {
            query: String::new(),
            cursor: None,
            anchor: None,
            limit: 100,
        };
        let mut seen = Vec::new();
        loop {
            let before = query.position("epoch", 1, now).unwrap();
            let records = db.query("", before, 100, &[]).await.unwrap();
            let page = history::page(records, vec![], &query, before, "epoch", 1, now).unwrap();
            assert!(
                page.records
                    .iter()
                    .map(|r| serde_json::to_vec(r).unwrap().len())
                    .sum::<usize>()
                    <= PAGE_BYTES
            );
            assert!(!page.records.is_empty());
            for record in page.records {
                assert!(!seen.contains(&record.notification.id));
                seen.push(record.notification.id);
            }
            query.cursor = page.next_cursor;
            if query.cursor.is_none() {
                break;
            }
            assert!(seen.len() < 12);
        }
        assert_eq!(seen.len(), 12);
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn center_does_not_decode_unselected_payloads_and_search_is_literal() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        let n = notification(1);
        db.apply(Mutation::Save(Box::new(n.clone()))).await.unwrap();
        assert_eq!(
            db.query("éclair 100%_", None, 10, &[]).await.unwrap().len(),
            1
        );
        assert!(db.query("missing", None, 10, &[]).await.unwrap().is_empty());
        db.connection
            .execute("UPDATE notifications SET payload_json='invalid-json'", ())
            .await
            .unwrap();
        let page = db
            .center(&query(serde_json::json!({"view":"apps"})), &[], "epoch", 1)
            .await
            .unwrap();
        let CenterPage::Apps { apps, .. } = page else {
            panic!()
        };
        assert_eq!(apps[0].count, 1);
        let selected = query(
            serde_json::json!({"view":"app", "selected":{"id":1,"created":n.created_unix_ms}}),
        );
        assert!(
            db.center(&selected, &[], "epoch", 1).await.is_err(),
            "only selected detail reads full payload"
        );
        db.checkpoint().await.unwrap();
    });
}

#[test]
#[ignore = "large-window performance qualification; run explicitly without competing tests"]
fn complete_window_retention_targets_and_bounded_refresh() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut db = NotificationStore::open(&path).await.unwrap();
        let start = std::time::Instant::now();
        db.begin().await.unwrap();
        // Prepared fixture seeding avoids benchmarking parser overhead on 5,005 writes.
        let mut insert = db.connection.prepare("INSERT INTO notifications(session_id,payload_json,preview_json,app_key,search_text,created_unix_ms,updated_unix_ms) VALUES (?1,?2,?3,?4,?5,?6,?6)").await.unwrap();
        let now = crate::time::unix_ms();
        for id in 1..=SCOPE_LIMIT + 5 {
            let mut n = notification(u32::try_from(id).unwrap());
            n.created_unix_ms = now;
            n.body = "x".repeat(16 * 1024);
            let preview = StoredPreview::from_active(&n);
            insert
                .execute(params![
                    i64::try_from(id).unwrap(),
                    serde_json::to_string(&n).unwrap(),
                    serde_json::to_string(&preview).unwrap(),
                    n.app_key(),
                    search_text(&n),
                    integer(now).unwrap()
                ])
                .await
                .unwrap();
        }
        drop(insert);
        db.remember_id(100_000).await.unwrap();
        db.finish(Ok(())).await.unwrap();
        let seeded = start.elapsed();
        let start = std::time::Instant::now();
        let CenterPage::Apps { apps, .. } = db
            .center(&query(serde_json::json!({"view":"apps"})), &[], "epoch", 1)
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(apps[0].count, SCOPE_LIMIT);
        eprintln!(
            "notification window: seed={seeded:?}, 5000-preview refresh={:?}",
            start.elapsed()
        );
        let targets = db
            .targets(Some(&notification(1).app_key()), None)
            .await
            .unwrap();
        assert_eq!(
            targets.len(),
            SCOPE_LIMIT + 5,
            "deletion includes rows outside the search window"
        );
        assert_eq!(
            db.prune().await.unwrap(),
            0,
            "count retention protects live rows"
        );
        db.connection
            .execute(
                "UPDATE notifications SET closed_unix_ms=?1,close_reason=2 WHERE session_id<>1",
                [integer(now).unwrap()],
            )
            .await
            .unwrap();
        assert_eq!(db.prune().await.unwrap(), 4);
        assert_eq!(db.targets(None, None).await.unwrap().len(), SCOPE_LIMIT + 1);
        assert_eq!(db.last_id().await.unwrap(), 100_000);
        db.checkpoint().await.unwrap();
        drop(db);
        let mut db = NotificationStore::open(&path).await.unwrap();
        db.apply(Mutation::Clear {
            closed_unix_ms: now,
            reason: 4,
        })
        .await
        .unwrap();
        assert_eq!(db.prune().await.unwrap(), 1);
        assert_eq!(db.last_id().await.unwrap(), 100_000);
        // Deleting everything must not reuse durable history identities.
        let previous = db
            .integer("SELECT max(history_id) FROM notifications")
            .await
            .unwrap();
        db.delete(&db.targets(None, None).await.unwrap())
            .await
            .unwrap();
        db.apply(Mutation::Save(Box::new(notification(100_001))))
            .await
            .unwrap();
        assert!(db.list(None, 1).await.unwrap()[0].history_id > previous);
        db.checkpoint().await.unwrap();
    });
}
