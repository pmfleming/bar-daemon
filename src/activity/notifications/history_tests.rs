use super::{
    engine::NotificationEngine,
    history::{HistoryError, HistoryQuery},
    model::IncomingNotification,
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
    let (_dir, path, engine) = NotificationEngine::persistent_fixture().await;
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
async fn failed_persistence_never_labels_old_content_with_a_new_revision() {
    let (_dir, path, engine) = NotificationEngine::persistent_fixture().await;
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
