use super::*;
use crate::activity::notifications::{
    database::Fault, model::ActiveNotification, settings::Settings,
};
use tempfile::tempdir;

fn notification(id: u32, transient: bool) -> ActiveNotification {
    let mut n = super::super::model::incoming("test", &format!("notification {id}"), "");
    n.hints.transient = transient;
    ActiveNotification::from_incoming(id, n, crate::time::unix_ms())
}

#[test]
fn turso_read_recovery_classifies_real_contention_and_restores_timeout() {
    let dir = tempdir().unwrap();
    let db = block_on(NotificationStore::open(&dir.path().join("history.db"))).unwrap();
    let blocker = db.competing_connection();
    block_on(blocker.execute("BEGIN IMMEDIATE", ())).unwrap();
    let mut attempts = 0;
    // WAL normally permits readers during a writer. Use a write-reserved read
    // transaction to exercise the retry policy with a real engine Busy error.
    let value = recover_center_read(
        &db.connection,
        || false,
        || {
            attempts += 1;
            let result = block_on(async {
                db.connection.execute("BEGIN IMMEDIATE", ()).await?;
                let value = db.integer("SELECT count(*) FROM notifications").await;
                db.connection.execute("ROLLBACK", ()).await?;
                value
            });
            if attempts == 1 {
                assert!(transient_read_error(result.as_ref().unwrap_err()));
                block_on(blocker.execute("ROLLBACK", ())).unwrap();
            }
            result
        },
    )
    .unwrap();
    assert_eq!(value, 0);
    assert_eq!(attempts, 2);
    assert_eq!(block_on(db.integer("PRAGMA busy_timeout")).unwrap(), 5000);
    let mut attempts = 0;
    let error = recover_center_read(
        &db.connection,
        || false,
        || -> Result<()> {
            attempts += 1;
            Err(turso::Error::BusySnapshot("snapshot".into()).into())
        },
    )
    .unwrap_err();
    assert!(transient_read_error(&error));
    assert_eq!(attempts, 3);
    let result = recover_center_read(
        &db.connection,
        || true,
        || -> Result<()> { panic!("cancelled read ran") },
    );
    assert!(result.is_err());
    assert_eq!(block_on(db.integer("PRAGMA busy_timeout")).unwrap(), 5000);
    block_on(db.checkpoint()).unwrap();
}

#[tokio::test]
async fn age_retention_settings_dnd_and_ids_survive_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("history.db");
    let (store, _, _, _) = NotificationPersistence::open(&path).await.unwrap();
    let mut old = notification(1, false);
    old.created_unix_ms -= 2 * 86_400_000;
    {
        let mut writes = store.reserve().await.unwrap();
        writes.save(old);
        writes.save(notification(2, false));
        writes.save(notification(100_000, true));
        writes.set_dnd(true, None);
    }
    let settings = Settings {
        history_days: 1,
        countdown: false,
        code_expiry_seconds: 45,
    };
    store.settings(Some(settings.clone())).await.unwrap();
    assert_eq!(store.list(None, 10).await.unwrap().len(), 1);
    store.shutdown().await.unwrap();
    let (store, id, dnd, until) = NotificationPersistence::open(&path).await.unwrap();
    assert_eq!((id, dnd, until), (100_000, true, None));
    assert_eq!(store.settings(None).await.unwrap(), settings);
    let rows = store.list(None, 10).await.unwrap();
    assert_eq!(rows[0].notification.id, 2);
    assert!(rows[0].closed_unix_ms.is_some());
    store.shutdown().await.unwrap();
}

#[test]
fn failed_pruning_rolls_back_settings() {
    block_on(async {
        let directory = tempdir().unwrap();
        let mut store = NotificationStore::open(&directory.path().join("history.db"))
            .await
            .unwrap();
        let mut old = notification(1, false);
        old.created_unix_ms -= 2 * 86_400_000;
        store.apply(Mutation::Save(Box::new(old))).await.unwrap();
        store.fault = Some(Fault::Prune);
        assert!(
            store
                .settings(Some(Settings {
                    history_days: 1,
                    ..Default::default()
                }))
                .await
                .is_err()
        );
        assert_eq!(store.settings(None).await.unwrap().history_days, 30);
        assert_eq!(store.list(None, 10).await.unwrap().len(), 1);
        store.checkpoint().await.unwrap();
    });
}

#[tokio::test]
async fn failed_write_fences_queries_but_continues_accepted_mutations() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("history.db");
    let (store, _, _, _) = NotificationPersistence::open(&path).await.unwrap();
    store.reserve().await.unwrap().save(notification(1, false));
    assert_eq!(store.list(None, 10).await.unwrap().len(), 1);
    store.inject_fault(Fault::Save).await;
    {
        let mut writes = store.reserve().await.unwrap();
        writes.save(notification(1, true));
        writes.save(notification(2, false));
        writes.set_dnd(true, None);
    }
    assert_eq!(store.list(None, 10).await.unwrap().len(), 2);
    let first = store
        .query(String::new(), None, 10, Vec::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(first.contains("injected notification Save failure"));
    store
        .reserve()
        .await
        .unwrap()
        .close(2, crate::time::unix_ms(), 2);
    assert_eq!(
        store
            .query(String::new(), None, 10, Vec::new())
            .await
            .unwrap_err()
            .to_string(),
        first
    );
    assert_eq!(store.list(None, 10).await.unwrap()[0].close_reason, Some(2));
    assert!(
        store.shutdown().await.is_err(),
        "shutdown reports lost writes but releases ownership"
    );
    let (reopened, _, dnd, _) = NotificationPersistence::open(&path).await.unwrap();
    assert!(dnd);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_reservations_and_mutations_do_not_lose_accepted_writes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("history.db");
    let db = block_on(NotificationStore::open(&path)).unwrap();
    let (commands, receiver) = mpsc::channel(1);
    let persistence = NotificationPersistence { commands };
    persistence
        .reserve()
        .await
        .unwrap()
        .save(notification(1, false));
    {
        let reserve = persistence.reserve();
        tokio::pin!(reserve);
        assert!(futures::poll!(&mut reserve).is_pending());
    }
    let worker = std::thread::spawn(move || persistence_worker(db, receiver));
    let (changed, received) = oneshot::channel();
    let pending = persistence.clone();
    let mutation = tokio::spawn(async move {
        let mut writes = pending.reserve().await.unwrap();
        writes.close(1, 200, 2);
        writes.set_dnd(true, None);
        changed.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    received.await.unwrap();
    mutation.abort();
    assert!(mutation.await.unwrap_err().is_cancelled());
    assert_eq!(
        persistence.list(None, 10).await.unwrap()[0].close_reason,
        Some(2)
    );
    persistence.shutdown().await.unwrap();
    worker.join().unwrap();
    let (store, _, dnd, _) = NotificationPersistence::open(&path).await.unwrap();
    assert!(dnd);
    assert!(
        store.list(None, 10).await.unwrap()[0]
            .closed_unix_ms
            .is_some()
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_reserved_writes_and_releases_ownership_before_reply() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("history.db");
    let (store, _, _, _) = NotificationPersistence::open(&path).await.unwrap();
    let mut writes = store.reserve().await.unwrap();
    let closing = store.clone();
    let shutdown = tokio::spawn(async move { closing.shutdown().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !store.commands.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!shutdown.is_finished());
    writes.save(notification(1, false));
    drop(writes);
    shutdown.await.unwrap().unwrap();
    assert!(store.reserve().await.is_err());
    let (reopened, id, _, _) = NotificationPersistence::open(&path).await.unwrap();
    assert_eq!(id, 1);
    assert_eq!(reopened.list(None, 10).await.unwrap().len(), 1);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_once_waiter_still_commits_and_deduplicates() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("history.db");
    let db = block_on(NotificationStore::open(&path)).unwrap();
    let (commands, receiver) = mpsc::channel(1);
    let persistence = NotificationPersistence { commands };
    let (response, waiter) = oneshot::channel();
    persistence
        .commands
        .send(PersistenceCommand::Once {
            key: "event".into(),
            notification: Box::new(notification(1, false)),
            response,
        })
        .await
        .unwrap();
    drop(waiter);
    let worker = std::thread::spawn(move || persistence_worker(db, receiver));
    assert!(
        !persistence
            .save_once("event".into(), notification(2, false))
            .await
            .unwrap()
    );
    assert_eq!(
        persistence.list(None, 10).await.unwrap()[0].notification.id,
        1
    );
    persistence.shutdown().await.unwrap();
    worker.join().unwrap();
}
