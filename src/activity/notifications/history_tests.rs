use super::{
    engine::NotificationEngine,
    history::{self, HistoryError, HistoryQuery, MAX_PAGE, PAGE_BYTES, SCOPE_LIMIT},
    model::{ActiveNotification, IncomingNotification},
};
use crate::state::StateStore;

fn notification(summary: &str, transient: bool) -> IncomingNotification {
    let mut n = super::model::incoming("Chat", summary, "");
    n.hints.transient = transient;
    n
}
fn query(text: &str, cursor: Option<String>, limit: usize) -> HistoryQuery {
    HistoryQuery {
        query: text.into(),
        cursor,
        limit,
        anchor: None,
    }
}

#[tokio::test]
async fn catalog_search_pages_and_mutation_restart_fences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    let first = engine
        .notify(0, notification("ÉCLAIR 100%_", false))
        .await
        .unwrap();
    engine.dismiss(first).await.unwrap();
    for index in 0..65 {
        engine
            .notify(0, notification(&format!("Message {index}"), false))
            .await
            .unwrap();
    }
    let transient = engine
        .notify(0, notification("Transient", true))
        .await
        .unwrap();
    let found = engine
        .query_history(query("  éclair 100%_  ", None, 2))
        .await
        .unwrap();
    assert_eq!(found.query, "éclair 100%_");
    assert_eq!(
        found.records.len(),
        1,
        "search covers records beyond the first page; wildcards are literal"
    );
    assert_eq!(found.records[0].notification.id, first);
    assert!(found.records[0].closed_unix_ms.is_some());
    let page = engine.query_history(query("", None, 2)).await.unwrap();
    assert_eq!(page.records[0].notification.id, transient);
    assert_eq!(page.records[0].history_id, None);
    let token = page.next_cursor.clone().unwrap();
    assert!(matches!(
        engine
            .query_history(query("other", Some(token.clone()), 2))
            .await,
        Err(HistoryError::Stale)
    ));
    engine.set_dnd(true, None).await.unwrap();
    let next = engine
        .query_history(query("", Some(token.clone()), 2))
        .await
        .unwrap();
    assert_eq!(
        page.revision, next.revision,
        "DND does not invalidate catalog pages"
    );
    assert!(next.records.iter().all(|item| {
        !page
            .records
            .iter()
            .any(|old| old.notification.id == item.notification.id)
    }));
    engine.dismiss(transient).await.unwrap();
    assert!(matches!(
        engine
            .query_history(query("", Some(token.clone()), 2))
            .await,
        Err(HistoryError::Stale)
    ));
    let restarted = NotificationEngine::persistent(StateStore::default(), path)
        .await
        .unwrap();
    assert!(matches!(
        restarted.query_history(query("", Some(token), 2)).await,
        Err(HistoryError::Stale)
    ));
    assert!(
        restarted
            .query_history(query("transient", None, 2))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        restarted
            .query_history(query("", None, 100))
            .await
            .unwrap()
            .records
            .iter()
            .all(|item| item.closed_unix_ms.is_some())
    );
}

#[tokio::test]
async fn transient_replacement_close_is_authoritative_without_intermediate_reads() {
    let dir = tempfile::tempdir().unwrap();
    let engine =
        NotificationEngine::persistent(StateStore::default(), dir.path().join("history.db"))
            .await
            .unwrap();
    let id = engine
        .notify(0, notification("Persisted", false))
        .await
        .unwrap();
    let old = engine.query_history(query("", None, 50)).await.unwrap();
    assert_eq!(
        old.records.len(),
        1,
        "live/history copies are deduplicated in Rust"
    );
    engine
        .notify(id, notification("Secret transient", true))
        .await
        .unwrap();
    engine.dismiss(id).await.unwrap();
    let current = engine.query_history(query("", None, 50)).await.unwrap();
    assert!(current.records.is_empty());
    assert_ne!(current.revision, old.revision);
    assert!(engine.history(None, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn bounded_pages_make_progress_and_reject_invalid_cursors() {
    let engine = NotificationEngine::new(StateStore::default()).await;
    for _ in 0..20 {
        let mut incoming = notification("Large", true);
        incoming.body = "x".repeat(64 * 1024);
        engine.notify(0, incoming).await.unwrap();
    }
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let page = engine
            .query_history(query("", cursor, MAX_PAGE))
            .await
            .unwrap();
        let bytes: usize = page
            .records
            .iter()
            .map(|item| serde_json::to_vec(item).unwrap().len())
            .sum();
        assert!(bytes <= PAGE_BYTES);
        assert!(!page.records.is_empty());
        ids.extend(page.records.iter().map(|item| item.notification.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 20);
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 20);
    for invalid in [
        query("", None, 0),
        query("", None, MAX_PAGE + 1),
        query(&"x".repeat(1025), None, 1),
        query("", Some("not a cursor".into()), 1),
    ] {
        assert!(matches!(
            engine.query_history(invalid).await,
            Err(HistoryError::Invalid)
        ));
    }
    let page = engine.query_history(query("", None, 1)).await.unwrap();
    let mut token: serde_json::Value = serde_json::from_str(&page.next_cursor.unwrap()).unwrap();
    token["expires"] = 0.into();
    assert!(matches!(
        engine
            .query_history(query("", Some(token.to_string()), 1))
            .await,
        Err(HistoryError::Stale)
    ));
    // Generated cursors must accept every valid query, including JSON escaping.
    let escaped = "\u{1}".repeat(1024);
    for _ in 0..2 {
        engine
            .notify(0, notification(&escaped, true))
            .await
            .unwrap();
    }
    let escaped_page = engine
        .query_history(query(&escaped, None, 1))
        .await
        .unwrap();
    assert!(escaped_page.next_cursor.as_ref().unwrap().len() > 4096);
    assert_eq!(
        engine
            .query_history(query(&escaped, escaped_page.next_cursor, 1))
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    assert_ne!(history::new_epoch().unwrap(), history::new_epoch().unwrap());
}

#[test]
fn refresh_anchor_completes_even_when_the_oldest_visible_record_was_deleted() {
    let active = vec![
        ActiveNotification::from_incoming(3, notification("new", true), 300),
        ActiveNotification::from_incoming(1, notification("old", true), 100),
    ];
    let mut request = query("", None, 1);
    request.anchor = Some(history::Position {
        created: 200,
        id: 2,
    });
    let first =
        history::page(Vec::new(), active.clone(), &request, None, "epoch", 7, 1000).unwrap();
    assert!(!first.anchor_reached);
    request.cursor = first.next_cursor;
    let position = request.position("epoch", 7, 1001).unwrap();
    let last = history::page(Vec::new(), active, &request, position, "epoch", 7, 1001).unwrap();
    assert!(last.anchor_reached);
    assert_eq!(last.records[0].notification.id, 1);
}

#[tokio::test]
async fn failed_persistence_never_labels_old_content_with_a_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    let id = engine
        .notify(0, notification("Persisted", false))
        .await
        .unwrap();
    engine.query_history(query("", None, 10)).await.unwrap();
    let connection = rusqlite::Connection::open(path).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON notifications BEGIN SELECT RAISE(FAIL, 'simulated disk failure'); END;").unwrap();
    engine
        .notify(id, notification("Transient", true))
        .await
        .unwrap();
    engine.dismiss(id).await.unwrap();
    assert!(matches!(
        engine.query_history(query("", None, 10)).await,
        Err(HistoryError::Unavailable(_))
    ));
}

#[tokio::test]
async fn rollback_legacy_writes_are_reindexed_on_upgrade_without_losing_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
        .await
        .unwrap();
    let id = engine
        .notify(0, notification("Original", false))
        .await
        .unwrap();
    engine.dismiss(id).await.unwrap();
    let mut old = engine
        .history(None, 1)
        .await
        .unwrap()
        .remove(0)
        .notification;
    drop(engine);
    old.summary = "Résumé".into();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE notifications SET payload_json = ?1 WHERE session_id = ?2",
            rusqlite::params![serde_json::to_string(&old).unwrap(), id],
        )
        .unwrap();
    let inserted = ActiveNotification::from_incoming(
        999,
        notification("Legacy insert", false),
        old.created_unix_ms + 1,
    );
    connection.execute("INSERT INTO notifications(session_id,payload_json,created_unix_ms,updated_unix_ms) VALUES (?1,?2,?3,?3)", rusqlite::params![inserted.id, serde_json::to_string(&inserted).unwrap(), inserted.created_unix_ms]).unwrap();
    drop(connection);
    let engine = NotificationEngine::persistent(StateStore::default(), path)
        .await
        .unwrap();
    assert_eq!(
        engine
            .query_history(query("résumé", None, 10))
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    assert_eq!(
        engine
            .query_history(query("legacy insert", None, 10))
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    assert!(
        engine
            .query_history(query("original", None, 10))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    let live = engine
        .notify(0, notification("ÉCLAIR", false))
        .await
        .unwrap();
    engine
        .notify(live, notification("éclair", false))
        .await
        .unwrap();
    engine.dismiss(live).await.unwrap();
    assert_eq!(
        engine
            .query_history(query("éclair", None, 10))
            .await
            .unwrap()
            .records
            .len(),
        1,
        "same normalized text must survive payload-only invalidation"
    );
}

#[tokio::test]
async fn legacy_database_migration_search_scope_and_stale_replacements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE notifications (history_id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, payload_json TEXT NOT NULL, created_unix_ms INTEGER NOT NULL, updated_unix_ms INTEGER NOT NULL, closed_unix_ms INTEGER, close_reason INTEGER);").unwrap();
    let transaction = connection.transaction().unwrap();
    for id in 1..=SCOPE_LIMIT + 1 {
        let item = ActiveNotification::from_incoming(
            id as u32,
            notification(if id == 1 { "outside scope" } else { "ÉCLAIR" }, false),
            id as u64,
        );
        transaction.execute("INSERT INTO notifications(session_id,payload_json,created_unix_ms,updated_unix_ms,closed_unix_ms) VALUES (?1,?2,?1,?1,99999)", rusqlite::params![id, serde_json::to_string(&item).unwrap()]).unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let engine = NotificationEngine::persistent(StateStore::default(), path)
        .await
        .unwrap();
    assert!(
        engine
            .query_history(query("outside scope", None, 10))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    let page = engine
        .query_history(query("éclair", None, 10))
        .await
        .unwrap();
    assert_eq!(page.records.len(), 10);
    assert_eq!(page.scope_limit, SCOPE_LIMIT);
    let id = engine
        .notify(0, notification("Original", false))
        .await
        .unwrap();
    let before = engine.query_history(query("", None, 1)).await.unwrap();
    engine
        .notify(id, notification("Replaced", false))
        .await
        .unwrap();
    assert!(matches!(
        engine.query_history(query("", before.next_cursor, 1)).await,
        Err(HistoryError::Stale)
    ));
    assert!(
        engine
            .query_history(query("original", None, 10))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    assert_eq!(
        engine
            .query_history(query("replaced", None, 10))
            .await
            .unwrap()
            .records
            .len(),
        1
    );
}
