//! Real-engine qualification, isolated from all live notification stores.
use std::{path::PathBuf, process::Command, time::Duration};

use futures::executor::block_on;

use super::{
    database::{Fault, NotificationStore},
    model::ActiveNotification,
    persistence::Mutation,
};

fn notification(id: u32) -> ActiveNotification {
    ActiveNotification::from_incoming(
        id,
        super::model::incoming("Chat", "Durable", "body"),
        crate::time::unix_ms(),
    )
}

#[test]
fn pragmas_constraints_integer_ranges_and_transaction_fences() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let mut db = NotificationStore::open(&path).await.unwrap();
        assert_eq!(db.integer("PRAGMA synchronous").await.unwrap(), 2);
        let journal: String = db
            .connection
            .query("PRAGMA journal_mode", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        assert!(
            db.connection
                .execute(
                    "INSERT INTO notification_once VALUES ('bad','not an integer')",
                    ()
                )
                .await
                .is_err()
        );
        let mut overflow = notification(1);
        overflow.created_unix_ms = u64::MAX;
        assert!(db.apply(Mutation::Save(Box::new(overflow))).await.is_err());
        assert_eq!(db.last_id().await.unwrap(), 0);
        assert!(db.connection.is_autocommit().unwrap());
        db.fault = Some(Fault::Commit);
        assert!(
            db.apply(Mutation::Save(Box::new(notification(1))))
                .await
                .is_err()
        );
        assert!(db.blocked, "ambiguous commit fences the connection");
        assert!(db.list(None, 10).await.is_err());
        assert!(
            db.apply(Mutation::Save(Box::new(notification(2))))
                .await
                .is_err()
        );
        assert!(db.checkpoint().await.is_err());
        drop(db);
        let mut db = NotificationStore::open(&path).await.unwrap();
        assert_eq!(
            db.list(None, 10).await.unwrap().len(),
            1,
            "post-commit failure did not lose or replay the commit"
        );
        db.begin().await.unwrap();
        db.connection
            .execute("UPDATE notifications SET summary='uncommitted'", ())
            .await
            .unwrap();
        db.fault = Some(Fault::Rollback);
        assert!(
            db.finish::<()>(Err(anyhow::anyhow!("operation failed")))
                .await
                .is_err()
        );
        assert!(db.blocked);
        assert!(!db.connection.is_autocommit().unwrap());
        assert!(
            db.list(None, 10).await.is_err(),
            "never expose a failed transaction's own writes"
        );
        assert!(db.checkpoint().await.is_err());
        drop(db);
        let db = NotificationStore::open(&path).await.unwrap();
        assert_eq!(
            db.list(None, 10).await.unwrap()[0].notification.summary,
            "Durable"
        );
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn refuses_newer_schema_and_symlinked_files_without_touching_targets() {
    block_on(async {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let db = NotificationStore::open(&path).await.unwrap();
        db.connection
            .execute("PRAGMA user_version=99", ())
            .await
            .unwrap();
        db.checkpoint().await.unwrap();
        drop(db);
        assert!(NotificationStore::open(&path).await.is_err());
        let target = dir.path().join("untouched");
        std::fs::write(&target, b"leave me alone").unwrap();
        let link = dir.path().join("symlink.db");
        symlink(&target, &link).unwrap();
        assert!(NotificationStore::open(&link).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"leave me alone");
        let missing = dir.path().join("must-not-create");
        let fresh = dir.path().join("orphan.db");
        symlink(&missing, dir.path().join("orphan.db-wal")).unwrap();
        assert!(NotificationStore::open(&fresh).await.is_err());
        assert!(!missing.exists());
    });
}

#[test]
fn once_key_and_history_rollback_together_and_survive_deletion() {
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        db.fault = Some(Fault::Children);
        assert!(db.save_once("event", &notification(1)).await.is_err());
        assert!(db.save_once("event", &notification(1)).await.unwrap());
        assert!(!db.save_once("event", &notification(2)).await.unwrap());
        db.delete(&db.targets(None, None).await.unwrap())
            .await
            .unwrap();
        assert!(!db.save_once("event", &notification(3)).await.unwrap());
        assert!(db.list(None, 10).await.unwrap().is_empty());
        db.checkpoint().await.unwrap();
    });
}

const CHILD: &str = "BAR_DAEMON_NOTIFICATION_TEST_CHILD";
const ROOT: &str = "BAR_DAEMON_NOTIFICATION_TEST_ROOT";
const ENTRY: &str = "activity::notifications::qualification_tests::subprocess_entry";

#[test]
fn subprocess_entry() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    if mode == "cutover" || mode == "reboot" {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let path = crate::activity::config::notification_database_path();
                assert_eq!(path, root.join("bar-daemon/notifications-turso-v2.db"));
                let (store, last_id, dnd, until) =
                    super::persistence::NotificationPersistence::open(&path)
                        .await
                        .unwrap();
                if mode == "cutover" {
                    assert_eq!((last_id, dnd, until), (0, false, None));
                    assert!(store.list(None, 10).await.unwrap().is_empty());
                    let mut writes = store.reserve().await.unwrap();
                    writes.save(notification(1));
                    writes.set_dnd(true, None);
                    drop(writes);
                    store
                        .settings(Some(super::settings::Settings {
                            history_days: 7,
                            ..Default::default()
                        }))
                        .await
                        .unwrap();
                } else {
                    assert_eq!((last_id, dnd, until), (1, true, None));
                    assert_eq!(store.settings(None).await.unwrap().history_days, 7);
                    let rows = store.list(None, 10).await.unwrap();
                    assert_eq!(rows.len(), 1, "reboot must reopen, not reset the new store");
                    assert_eq!(rows[0].notification.id, 1);
                    assert!(
                        rows[0].closed_unix_ms.is_some(),
                        "old sessions become closed history"
                    );
                }
                store.shutdown().await.unwrap();
            });
        return;
    }
    block_on(async {
        let path = root.join("history.db");
        let mut db = NotificationStore::open(&path).await.unwrap();
        db.apply(Mutation::Save(Box::new(notification(1))))
            .await
            .unwrap();
        if mode == "crash" {
            db.begin().await.unwrap();
            db.connection
                .execute("UPDATE notifications SET summary='uncommitted'", ())
                .await
                .unwrap();
            // No destructors/checkpoint/normal shutdown. OS releases ownership.
            std::process::exit(86);
        }
        db.checkpoint().await.unwrap();
    });
}

fn child(root: &std::path::Path, mode: &str) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", ENTRY, "--nocapture"])
        .env(CHILD, mode)
        .env(ROOT, root)
        .env("XDG_STATE_HOME", root)
        .env_remove("BAR_DAEMON_NOTIFICATION_DATABASE")
        .output()
        .unwrap()
}

#[test]
fn abrupt_exit_recovers_committed_wal_and_discards_uncommitted_changes() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "crash");
    assert_eq!(
        output.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::metadata(dir.path().join("history.db-wal"))
            .unwrap()
            .len()
            > 32
    );
    block_on(async {
        let db = NotificationStore::open(&dir.path().join("history.db"))
            .await
            .unwrap();
        let rows = db.list(None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].notification.summary, "Durable");
        assert_eq!(db.last_id().await.unwrap(), 1);
        db.checkpoint().await.unwrap();
    });
}

#[test]
fn default_cutover_and_reboot_ignore_legacy_and_reopen_turso_state() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("bar-daemon");
    std::fs::create_dir(&old).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        std::fs::write(
            old.join(format!("notifications.sqlite3{suffix}")),
            b"legacy bytes need not even be valid SQLite",
        )
        .unwrap();
        std::fs::write(
            old.join(format!("notifications-turso-v1.db{suffix}")),
            b"old Turso storage must not be opened",
        )
        .unwrap();
    }
    // Separate processes exercise the actual worker's startup/drain path, as
    // rebuild activation followed by a reboot would. Both see legacy files.
    for mode in ["cutover", "reboot"] {
        let output = child(dir.path(), mode);
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for suffix in ["", "-wal", "-shm"] {
        assert_eq!(
            std::fs::read(old.join(format!("notifications.sqlite3{suffix}"))).unwrap(),
            b"legacy bytes need not even be valid SQLite"
        );
        assert_eq!(
            std::fs::read(old.join(format!("notifications-turso-v1.db{suffix}"))).unwrap(),
            b"old Turso storage must not be opened"
        );
    }
    assert!(old.join("notifications-turso-v2.db").exists());
}

#[tokio::test]
async fn queue_drain_latency_and_shutdown_close_admission() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let (store, _, _, _) = super::persistence::NotificationPersistence::open(&path)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    for id in 1..=100 {
        store.reserve().await.unwrap().save(notification(id));
    }
    assert_eq!(store.list(None, 100).await.unwrap().len(), 100);
    eprintln!(
        "notification FULL-sync 100-command queue drain: {:?}",
        started.elapsed()
    );
    store.shutdown().await.unwrap();
    assert!(store.reserve().await.is_err());
    let (store, id, _, _) = super::persistence::NotificationPersistence::open(&path)
        .await
        .unwrap();
    assert_eq!(id, 100);
    assert!(
        store
            .list(None, 100)
            .await
            .unwrap()
            .iter()
            .all(|n| n.closed_unix_ms.is_some())
    );
    tokio::time::timeout(Duration::from_secs(10), store.shutdown())
        .await
        .unwrap()
        .unwrap();
}
