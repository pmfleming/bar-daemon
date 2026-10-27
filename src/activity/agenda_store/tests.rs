use super::{Store, database::APPLICATION_ID, model::*};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

fn item() -> NewItem {
    NewItem {
        kind: ItemKind::Task,
        title: "Preserve me".into(),
        notes: "plain\r\nnotes 🦀".into(),
        links: vec![Link {
            id: "link-1".into(),
            label: "reference".into(),
            url: "https://example.org".into(),
        }],
        schedule: Schedule {
            end_date: Some("2026-10-30".into()),
            ..Default::default()
        },
        event: None,
        task: Some(TaskDetails {
            priority: 9,
            checklist: vec![ChecklistStep {
                id: "step-1".into(),
                text: "independent".into(),
                checked: false,
            }],
            ..Default::default()
        }),
    }
}
#[tokio::test]
async fn crud_children_revisions_receipts_restart_export_restore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await.unwrap();
    let original = store.create("create-1".into(), item()).await.unwrap();
    assert_eq!(original.catalog_revision, 1);
    for name in ["agenda.db", "agenda.db-wal", "agenda.db.lock"] {
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        store.create("create-1".into(), item()).await.unwrap(),
        original
    );
    let mut changed = item();
    changed.title = "different fingerprint".into();
    assert!(
        store
            .create("create-1".into(), changed)
            .await
            .unwrap_err()
            .to_string()
            .contains("fingerprint")
    );
    let mut record = original.item.clone().unwrap();
    let invalid = ItemPatch {
        title: Some("must roll back".into()),
        schedule: Some(Schedule {
            start_date: Some("2026-11-01".into()),
            end_date: Some("2026-10-01".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        store
            .patch("bad".into(), record.id.clone(), 1, invalid)
            .await
            .is_err()
    );
    assert_eq!(
        store.get(record.id.clone()).await.unwrap(),
        Some(record.clone())
    );
    assert!(store.receipt("bad".into()).await.unwrap().is_none());
    let mut task = record.task.clone().unwrap();
    task.completed = true;
    let completed = store
        .patch(
            "complete".into(),
            record.id.clone(),
            1,
            ItemPatch {
                task: Some(task),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let next = completed.item.clone().unwrap();
    assert_eq!(next.schedule, record.schedule);
    assert_eq!(
        next.task.as_ref().unwrap().checklist,
        record.task.as_ref().unwrap().checklist
    );
    assert!(next.task.as_ref().unwrap().completed_unix_ms.is_some());
    assert_eq!(next.revision, 2);
    assert!(
        store
            .delete("stale".into(), record.id.clone(), 1)
            .await
            .is_err()
    );
    record = next;
    let export = dir.path().join("export.json");
    store.export(export.clone()).await.unwrap();
    assert_eq!(
        fs::metadata(&export).unwrap().permissions().mode() & 0o777,
        0o600
    );
    store.shutdown().await.unwrap();
    let store = Store::open(root.clone(), None).await.unwrap();
    assert_eq!(
        store.get(record.id.clone()).await.unwrap(),
        Some(record.clone())
    );
    assert_eq!(
        store.receipt("create-1".into()).await.unwrap(),
        Some(original)
    );
    let restored = Store::open(dir.path().join("restore"), None).await.unwrap();
    restored.restore(export.clone()).await.unwrap();
    assert_eq!(
        restored.snapshot().await.unwrap().items,
        store.snapshot().await.unwrap().items
    );
    assert_eq!(
        restored.receipt("complete".into()).await.unwrap(),
        Some(completed)
    );
    assert!(restored.restore(export).await.is_err());
    let deletion = store
        .delete("delete".into(), record.id.clone(), 2)
        .await
        .unwrap();
    assert!(deletion.item.is_none());
    assert_eq!(deletion.catalog_revision, 3);
    assert_eq!(
        store
            .delete("delete".into(), record.id.clone(), 2)
            .await
            .unwrap(),
        deletion
    );
    assert!(store.get(record.id).await.unwrap().is_none());
    store.shutdown().await.unwrap();
    restored.shutdown().await.unwrap();
    for name in ["agenda.db", "agenda.db.lock"] {
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[tokio::test]
async fn migration_actual_serialization_fidelity_idempotence_changed_and_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let legacy = dir.path().join("todos.json");
    let bytes = br#"[{"id":"old-id","source_id":"old-local","title":" old title ","completed":true,"priority":255,"due_unix_ms":null,"due_date":"2026-10-30","created_unix_ms":1234,"completed_unix_ms":5678},{"id":"timed","source_id":"local","title":"timed","completed":false,"priority":4,"due_unix_ms":1793100000000,"due_date":null,"created_unix_ms":42,"completed_unix_ms":null}]"#;
    fs::write(&legacy, bytes).unwrap();
    let store = Store::open(root.clone(), Some(legacy.clone()))
        .await
        .unwrap();
    let record = store.get("old-id".into()).await.unwrap().unwrap();
    assert_eq!(record.source_id, "old-local");
    assert_eq!(record.title, " old title ");
    assert_eq!(record.created_unix_ms, 1234);
    assert_eq!(record.task.unwrap().priority, 255);
    assert_eq!(record.schedule.start_date, None);
    assert_eq!(record.schedule.end_date.as_deref(), Some("2026-10-30"));
    let timed = store.get("timed".into()).await.unwrap().unwrap();
    assert_eq!(timed.schedule.start_unix_ms, None);
    assert_eq!(timed.schedule.end_unix_ms, Some(1793100000000));
    let backups: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("legacy-todos-"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        fs::metadata(backups[0].path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(fs::read(backups[0].path()).unwrap(), bytes);
    store
        .patch(
            "edit".into(),
            "old-id".into(),
            1,
            ItemPatch {
                title: Some("post migration edit".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store.shutdown().await.unwrap();
    let store = Store::open(root.clone(), Some(legacy.clone()))
        .await
        .unwrap();
    assert_eq!(store.snapshot().await.unwrap().items.len(), 2);
    assert_eq!(
        store.get("old-id".into()).await.unwrap().unwrap().title,
        "post migration edit"
    );
    store.shutdown().await.unwrap();
    fs::write(&legacy, b"[]").unwrap();
    assert!(Store::open(root, Some(legacy.clone())).await.is_err());
    fs::write(&legacy, b"not json").unwrap();
    assert!(
        Store::open(dir.path().join("corrupt-legacy"), Some(legacy.clone()))
            .await
            .is_err()
    );
    assert_eq!(fs::read(&legacy).unwrap(), b"not json");
    let missing = Store::open(dir.path().join("missing"), Some(dir.path().join("absent")))
        .await
        .unwrap();
    assert!(missing.snapshot().await.unwrap().items.is_empty());
    missing.shutdown().await.unwrap();
}

#[tokio::test]
async fn migration_atomic_failure_and_retry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let legacy = dir.path().join("todos.json");
    let store = Store::open(root.clone(), None).await.unwrap();
    let original = store
        .create("create".into(), item())
        .await
        .unwrap()
        .item
        .unwrap();
    store.shutdown().await.unwrap();
    // First imported row succeeds, second conflicts: the whole import must roll back.
    let make = |id: &str| serde_json::json!({"id":id,"source_id":"local","title":"legacy","completed":false,"priority":9,"due_unix_ms":null,"due_date":null,"created_unix_ms":5,"completed_unix_ms":null});
    fs::write(
        &legacy,
        serde_json::to_vec(&vec![make("first"), make(&original.id)]).unwrap(),
    )
    .unwrap();
    assert!(
        Store::open(root.clone(), Some(legacy.clone()))
            .await
            .is_err()
    );
    let store = Store::open(root.clone(), None).await.unwrap();
    assert_eq!(store.snapshot().await.unwrap().items.len(), 1);
    assert!(store.get("first".into()).await.unwrap().is_none());
    store.shutdown().await.unwrap();
    fs::write(&legacy, serde_json::to_vec(&vec![make("first")]).unwrap()).unwrap();
    let store = Store::open(root, Some(legacy)).await.unwrap();
    assert_eq!(store.snapshot().await.unwrap().items.len(), 2);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn lock_schema_corruption_permissions_and_sql_contract() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await.unwrap();
    assert!(Store::open(root.clone(), None).await.is_err());
    store
        .call(|db| {
            futures::executor::block_on(async {
                assert_eq!(db.integer("PRAGMA foreign_keys").await?, 1);
                assert_eq!(db.integer("PRAGMA synchronous").await?, 2);
                assert_eq!(db.integer("PRAGMA application_id").await?, APPLICATION_ID);
                assert!(
                    db.conn
                        .execute(
                            "INSERT INTO items VALUES('orphan','absent',NULL,'task',1,x'00')",
                            ()
                        )
                        .await
                        .is_err()
                );
                assert!(
                    db.conn
                        .execute("INSERT INTO sources VALUES('bad',2,0,0)", ())
                        .await
                        .is_err()
                );
                assert!(
                    db.conn
                        .execute("INSERT INTO sources VALUES('bad','no',0,0)", ())
                        .await
                        .is_err()
                );
                db.conn.execute("BEGIN IMMEDIATE", ()).await?;
                db.conn
                    .execute("INSERT INTO sources VALUES('rollback',0,0,0)", ())
                    .await?;
                db.conn.execute("ROLLBACK", ()).await?;
                assert_eq!(
                    db.integer("SELECT count(*) FROM sources WHERE id='rollback'")
                        .await?,
                    0
                );
                db.conn.execute("PRAGMA user_version=999", ()).await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    store.shutdown().await.unwrap();
    assert!(Store::open(root.clone(), None).await.is_err());
    assert!(fs::metadata(root.join("agenda.db")).unwrap().len() > 0);
    let corrupt = dir.path().join("corrupt");
    fs::create_dir(&corrupt).unwrap();
    fs::write(corrupt.join("agenda.db"), b"not a database").unwrap();
    assert!(Store::open(corrupt.clone(), None).await.is_err());
    assert_eq!(
        fs::read(corrupt.join("agenda.db")).unwrap(),
        b"not a database"
    );
    let denied = dir.path().join("not-a-dir");
    fs::write(&denied, b"file").unwrap();
    assert!(Store::open(denied, None).await.is_err());
    let symlink = dir.path().join("symlink");
    std::os::unix::fs::symlink(&root, &symlink).unwrap();
    assert!(Store::open(symlink, None).await.is_err());
}

#[tokio::test]
async fn readonly_source_and_invalid_child_changes_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("agenda"), None).await.unwrap();
    let record = store
        .create("create".into(), item())
        .await
        .unwrap()
        .item
        .unwrap();
    let mut task = record.task.clone().unwrap();
    task.checklist.push(task.checklist[0].clone());
    assert!(
        store
            .patch(
                "bad-child".into(),
                record.id.clone(),
                1,
                ItemPatch {
                    title: Some("not saved".into()),
                    task: Some(task),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(store.get(record.id.clone()).await.unwrap().unwrap(), record);
    let mut imported = record.clone();
    imported.id = "import:1".into();
    imported.source_id = "ics:1".into();
    imported.capabilities = Capabilities::default();
    store
        .call(move |db| {
            futures::executor::block_on(async {
                db.conn
                    .execute("INSERT INTO sources VALUES('ics:1',0,0,0)", ())
                    .await?;
                db.insert_record(&imported).await
            })
        })
        .await
        .unwrap();
    assert!(
        store
            .patch(
                "readonly".into(),
                "import:1".into(),
                1,
                ItemPatch {
                    title: Some("forbidden".into()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(
        store
            .delete("readonly-del".into(), "import:1".into(), 1)
            .await
            .is_err()
    );
    store.shutdown().await.unwrap();
}

#[test]
fn abrupt_child() {
    let Some(root) = std::env::var_os("AGENDA_ABRUPT_TEST_ROOT") else {
        return;
    };
    let mode = std::env::var("AGENDA_ABRUPT_TEST_MODE").unwrap();
    futures::executor::block_on(async {
        let store = Store::open(root.into(), None).await.unwrap();
        if mode == "commit" {
            store.create("durable".into(), item()).await.unwrap();
        } else if mode == "limited" {
            // The parent ignores SIGXFSZ before exec; EFBIG reaches the real Syscall VFS.
            rustix::process::setrlimit(
                rustix::process::Resource::Fsize,
                rustix::process::Rlimit {
                    current: Some(65536),
                    maximum: Some(65536),
                },
            )
            .unwrap();
            let mut large = item();
            large.notes = "x".repeat(260000);
            assert!(store.create("limited".into(), large).await.is_err());
        } else {
            store
                .call(|db| {
                    futures::executor::block_on(async {
                        db.conn.execute("BEGIN IMMEDIATE", ()).await?;
                        db.conn.execute("DELETE FROM items", ()).await?;
                        Ok(())
                    })
                })
                .await
                .unwrap();
        }
        // Exit with owned live connections: no Store drop, shutdown or checkpoint.
        std::process::exit(0);
    });
}
#[tokio::test]
async fn actual_abrupt_process_wal_recovery_before_and_after_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    for mode in ["commit", "uncommitted"] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "activity::agenda_store::tests::abrupt_child",
                "--nocapture",
            ])
            .env("AGENDA_ABRUPT_TEST_ROOT", &root)
            .env("AGENDA_ABRUPT_TEST_MODE", mode)
            .status()
            .unwrap();
        assert!(status.success());
        let store = Store::open(root.clone(), None).await.unwrap();
        let receipt = store.receipt("durable".into()).await.unwrap().unwrap();
        assert_eq!(
            store.snapshot().await.unwrap().items,
            vec![receipt.item.unwrap()]
        );
        store.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn real_file_write_exhaustion_no_success_or_partial_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await.unwrap();
    store.shutdown().await.unwrap();
    let status = Command::new("bash")
        .args(["-c", "trap '' XFSZ; exec \"$@\"", "agenda-write-exhaustion"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "activity::agenda_store::tests::abrupt_child",
            "--nocapture",
        ])
        .env("AGENDA_ABRUPT_TEST_ROOT", &root)
        .env("AGENDA_ABRUPT_TEST_MODE", "limited")
        .status()
        .unwrap();
    assert!(status.success());
    let store = Store::open(root, None).await.unwrap();
    assert!(store.snapshot().await.unwrap().items.is_empty());
    assert!(store.receipt("limited".into()).await.unwrap().is_none());
    store.create("after-recovery".into(), item()).await.unwrap();
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn unreadable_legacy_and_unwritable_export_fail_without_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("todos.json");
    fs::write(&legacy, b"[]").unwrap();
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o0)).unwrap();
    let result = Store::open(dir.path().join("agenda"), Some(legacy.clone())).await;
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(result.is_err());
    let store = Store::open(dir.path().join("agenda"), Some(legacy))
        .await
        .unwrap();
    assert!(
        store
            .export(std::path::PathBuf::from("/proc/agenda-not-writable.json"))
            .await
            .is_err()
    );
    store.create("still-healthy".into(), item()).await.unwrap();
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn commit_error_never_returns_success_and_transaction_rollback_is_real() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("agenda"), None).await.unwrap();
    store
        .call(|db| {
            futures::executor::block_on(async {
                db.conn.execute("BEGIN IMMEDIATE", ()).await?;
                db.conn
                    .execute("INSERT INTO sources VALUES('rolled-back',0,0,0)", ())
                    .await?;
                let failure: anyhow::Result<()> =
                    Err(anyhow::anyhow!("operation failed after first SQL write"));
                assert!(db.finish(failure).await.is_err());
                assert_eq!(
                    db.integer("SELECT count(*) FROM sources WHERE id='rolled-back'")
                        .await?,
                    0
                );
                // Actual engine COMMIT failure (no active transaction) cannot be optimistic success.
                assert!(db.finish(Ok(())).await.is_err());
                Ok(())
            })
        })
        .await
        .unwrap();
    assert!(store.create("after-error".into(), item()).await.is_err());
    assert!(store.shutdown().await.is_err());
    let recovered = Store::open(dir.path().join("agenda"), None).await.unwrap();
    recovered
        .create("after-reopen".into(), item())
        .await
        .unwrap();
    recovered.shutdown().await.unwrap();
}

#[tokio::test]
async fn rollback_failure_policy_fences_future_mutations_and_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("agenda"), None).await.unwrap();
    store
        .call(|db| {
            futures::executor::block_on(async {
                // Inject an actual engine ROLLBACK error into the production recovery policy.
                let rollback = db
                    .conn
                    .execute("ROLLBACK", ())
                    .await
                    .map(|_| ())
                    .map_err(Into::into);
                assert!(rollback.is_err());
                db.record_rollback_result(rollback);
                Ok(())
            })
        })
        .await
        .unwrap();
    assert!(store.create("fenced".into(), item()).await.is_err());
    assert!(store.receipt("fenced".into()).await.unwrap().is_none());
    assert!(store.shutdown().await.is_err());
    let recovered = Store::open(dir.path().join("agenda"), None).await.unwrap();
    recovered
        .create("after-reopen".into(), item())
        .await
        .unwrap();
    recovered.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_revision_writers_have_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("agenda"), None).await.unwrap();
    let record = store
        .create("create".into(), item())
        .await
        .unwrap()
        .item
        .unwrap();
    let (a, b) = tokio::join!(
        store.patch(
            "a".into(),
            record.id.clone(),
            1,
            ItemPatch {
                title: Some("a".into()),
                ..Default::default()
            }
        ),
        store.patch(
            "b".into(),
            record.id.clone(),
            1,
            ItemPatch {
                title: Some("b".into()),
                ..Default::default()
            }
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(store.snapshot().await.unwrap().catalog_revision, 2);
    let current = store.get(record.id).await.unwrap().unwrap();
    assert_eq!(current.revision, 2);
    assert_eq!(
        store.receipt("a".into()).await.unwrap().is_some(),
        a.is_ok()
    );
    assert_eq!(
        store.receipt("b".into()).await.unwrap().is_some(),
        b.is_ok()
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_async_waiter_does_not_lose_accepted_durable_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("agenda"), None).await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let paused = store.clone();
    let pause = tokio::spawn(async move {
        paused
            .call(move |_| {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    let writer = store.clone();
    let waiter = tokio::spawn(async move { writer.create("uncertain".into(), item()).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.sender.capacity() == 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    waiter.abort();
    release_tx.send(()).unwrap();
    pause.await.unwrap().unwrap();
    let result = store.receipt("uncertain".into()).await.unwrap().unwrap();
    assert_eq!(result.catalog_revision, 1);
    assert_eq!(
        store
            .get(result.item.unwrap().id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Preserve me"
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_logical_restore_rolls_back_all_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("original"), None)
        .await
        .unwrap();
    store.create("create".into(), item()).await.unwrap();
    let export = dir.path().join("export.json");
    store.export(export.clone()).await.unwrap();
    store.shutdown().await.unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();
    value["snapshot"]["items"][0]["links"][0]["url"] = "javascript:forbidden".into();
    fs::write(&export, serde_json::to_vec(&value).unwrap()).unwrap();
    let restored = Store::open(dir.path().join("restored"), None)
        .await
        .unwrap();
    assert!(restored.restore(export).await.is_err());
    assert!(restored.snapshot().await.unwrap().items.is_empty());
    assert_eq!(restored.snapshot().await.unwrap().catalog_revision, 0);
    assert!(restored.receipt("create".into()).await.unwrap().is_none());
    restored
        .create("valid-after-failure".into(), item())
        .await
        .unwrap();
    restored.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_persisted_payload_is_not_treated_as_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await.unwrap();
    store.create("create".into(), item()).await.unwrap();
    store
        .call(|db| {
            futures::executor::block_on(async {
                db.conn
                    .execute("UPDATE items SET payload=x'00'", ())
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    assert!(store.snapshot().await.is_err());
    store.shutdown().await.unwrap();
    assert!(Store::open(root.clone(), None).await.is_err());
    assert!(fs::metadata(root.join("agenda.db")).unwrap().len() > 0);
}

#[tokio::test]
async fn engine_large_blob_prepared_commit_rollback_checkpoint_contract() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("agenda"), None).await.unwrap();
    store.call(|db| futures::executor::block_on(async {
        db.conn.execute("CREATE TABLE engine_blob_contract(id INTEGER PRIMARY KEY, body BLOB NOT NULL) STRICT", ()).await?;
        let body = vec![0x42u8; 16 * 1024 * 1024];
        db.conn.execute("BEGIN IMMEDIATE", ()).await?;
        let mut statement = db.conn.prepare("INSERT INTO engine_blob_contract VALUES(1,?)").await?;
        statement.execute([body.as_slice()]).await?;
        db.conn.execute("COMMIT", ()).await?;
        db.conn.execute_batch("BEGIN IMMEDIATE; DELETE FROM engine_blob_contract; ROLLBACK;").await?;
        let mut rows = db.conn.query("SELECT body FROM engine_blob_contract", ()).await?;
        assert_eq!(rows.next().await?.unwrap().get::<Vec<u8>>(0)?, body); drop(rows); drop(statement);
        db.conn.execute("DROP TABLE engine_blob_contract", ()).await?;
        db.checkpoint().await?; Ok(())
    })).await.unwrap();
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn existing_only_never_initializes_missing_or_zero_schema_and_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    assert!(Store::open_existing(absent.clone()).await.is_err());
    assert!(!absent.exists());
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert!(Store::open_existing(empty.clone()).await.is_err());
    assert!(!empty.join("agenda.db").exists());
    fs::write(empty.join("agenda.db"), b"").unwrap();
    assert!(Store::open_existing(empty.clone()).await.is_err());
    assert_eq!(fs::metadata(empty.join("agenda.db")).unwrap().len(), 0);
    // A preexisting zero-byte DB might be truncated user data, not a new store.
    assert!(Store::open(empty.clone(), None).await.is_err());
    assert_eq!(fs::metadata(empty.join("agenda.db")).unwrap().len(), 0);
    let root = dir.path().join("valid");
    let store = Store::open(root.clone(), None).await.unwrap();
    let created = store.create("create".into(), item()).await.unwrap();
    store.shutdown().await.unwrap();
    let reader = Store::open_existing(root).await.unwrap();
    assert_eq!(
        reader.receipt("create".into()).await.unwrap(),
        Some(created)
    );
    assert!(reader.create("forbidden".into(), item()).await.is_err());
    reader.shutdown().await.unwrap();
}

#[test]
fn migration_abrupt_child() {
    let Some(root) = std::env::var_os("AGENDA_ABRUPT_MIGRATION_ROOT") else {
        return;
    };
    let legacy = std::env::var_os("AGENDA_ABRUPT_MIGRATION_LEGACY").unwrap();
    futures::executor::block_on(Store::open(root.into(), Some(legacy.into()))).unwrap();
    panic!("migration interruption fault point was not reached");
}

#[tokio::test]
async fn actual_migration_process_interruption_keeps_backup_and_retries_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let legacy = dir.path().join("todos.json");
    let bytes = br#"[{"id":"first","source_id":"local","title":"first","completed":true,"priority":255,"due_date":"2026-10-27","due_unix_ms":null,"created_unix_ms":12,"completed_unix_ms":13},{"id":"second","source_id":"local","title":"second","completed":false,"priority":7,"due_date":null,"due_unix_ms":null,"created_unix_ms":14,"completed_unix_ms":null}]"#;
    fs::write(&legacy, bytes).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "activity::agenda_store::tests::migration_abrupt_child",
            "--nocapture",
        ])
        .env("AGENDA_ABRUPT_MIGRATION_ROOT", &root)
        .env("AGENDA_ABRUPT_MIGRATION_LEGACY", &legacy)
        .env("AGENDA_TEST_INTERRUPT_MIGRATION", "1")
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read(&legacy).unwrap(), bytes);
    let backups: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("legacy-todos-"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), bytes);
    let inspected = Store::open(root.clone(), None).await.unwrap();
    assert!(inspected.snapshot().await.unwrap().items.is_empty());
    inspected.shutdown().await.unwrap();
    let retried = Store::open(root.clone(), Some(legacy.clone()))
        .await
        .unwrap();
    assert_eq!(retried.snapshot().await.unwrap().items.len(), 2);
    retried.shutdown().await.unwrap();
    let reopened = Store::open(root, Some(legacy)).await.unwrap();
    assert_eq!(reopened.snapshot().await.unwrap().items.len(), 2);
    assert_eq!(reopened.snapshot().await.unwrap().catalog_revision, 1);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn provider_catalog_invalidation_is_durable_without_mutating_items_or_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await.unwrap();
    let created = store.create("create".into(), item()).await.unwrap();
    assert_eq!(store.invalidate_catalog().await.unwrap(), 2);
    assert_eq!(
        store.snapshot().await.unwrap().items,
        vec![created.item.clone().unwrap()]
    );
    assert_eq!(store.receipt("create".into()).await.unwrap(), Some(created));
    store.shutdown().await.unwrap();
    let reopened = Store::open(root.clone(), None).await.unwrap();
    assert_eq!(reopened.snapshot().await.unwrap().catalog_revision, 2);
    reopened.shutdown().await.unwrap();
    let reader = Store::open_existing(root).await.unwrap();
    assert!(reader.invalidate_catalog().await.is_err());
    reader.shutdown().await.unwrap();
}

#[tokio::test]
async fn corrupt_receipts_are_rejected_by_lookup_startup_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    for case in [
        "request",
        "zero_revision",
        "future_revision",
        "no_outcome",
        "both_outcomes",
        "record",
        "deleted_id",
        "json",
        "fingerprint",
    ] {
        let root = dir.path().join(format!("source-{case}"));
        let store = Store::open(root.clone(), None).await.unwrap();
        let result = store.create("create".into(), item()).await.unwrap();
        let export_path = dir.path().join(format!("export-{case}.json"));
        store.export(export_path.clone()).await.unwrap();
        let mut export: serde_json::Value =
            serde_json::from_slice(&fs::read(&export_path).unwrap()).unwrap();
        let mut result = serde_json::to_value(result).unwrap();
        match case {
            "request" => result["request_id"] = "different".into(),
            "zero_revision" => result["catalog_revision"] = 0.into(),
            "future_revision" => result["catalog_revision"] = 2.into(),
            "no_outcome" => result["item"] = serde_json::Value::Null,
            "both_outcomes" => result["deleted_id"] = "deleted".into(),
            "record" => result["item"]["title"] = "".into(),
            "deleted_id" => {
                result["item"] = serde_json::Value::Null;
                result["deleted_id"] = "".into();
            }
            _ => {}
        }
        let hash = if case == "fingerprint" {
            "not-a-hash".to_owned()
        } else {
            export["receipts"][0][1].as_str().unwrap().to_owned()
        };
        let body = if case == "json" {
            vec![0]
        } else {
            serde_json::to_vec(&result).unwrap()
        };
        let db_hash = hash.clone();
        let db_body = body.clone();
        store.call(move |db| futures::executor::block_on(async {
            db.conn.execute("UPDATE operation_receipts SET fingerprint=?,payload=? WHERE request_id='create'", turso::params![db_hash, db_body]).await?;
            Ok(())
        })).await.unwrap();
        assert!(
            store.receipt("create".into()).await.is_err(),
            "lookup accepted {case}"
        );
        store.shutdown().await.unwrap();
        assert!(
            Store::open(root, None).await.is_err(),
            "startup accepted {case}"
        );
        export["receipts"][0][1] = hash.into();
        export["receipts"][0][2] = serde_json::to_value(body).unwrap();
        fs::write(&export_path, serde_json::to_vec(&export).unwrap()).unwrap();
        let restored = Store::open(dir.path().join(format!("restore-{case}")), None)
            .await
            .unwrap();
        assert!(
            restored.restore(export_path).await.is_err(),
            "restore accepted {case}"
        );
        let snapshot = restored.snapshot().await.unwrap();
        assert!(
            snapshot.items.is_empty(),
            "restore retained partial item for {case}"
        );
        assert_eq!(snapshot.catalog_revision, 0);
        assert!(restored.receipt("create".into()).await.unwrap().is_none());
        restored
            .create("after-failed-restore".into(), item())
            .await
            .unwrap();
        restored.shutdown().await.unwrap();
    }
}

#[test]
fn record_patch_policy_preserves_completion_history_and_independent_children() {
    use super::mutation::{create_record, patch_record};
    for (old_completed, old_time, completed, submitted_time, expected_time) in [
        (false, None, true, None, Some(20)),
        (true, Some(12), true, None, Some(12)),
        (true, None, true, None, None),
        (true, Some(12), true, Some(15), Some(15)),
        (true, Some(12), false, Some(12), None),
        (false, None, false, Some(12), None),
    ] {
        let mut record = create_record("local:test".into(), item(), 10);
        let old = record.task.as_mut().unwrap();
        old.completed = old_completed;
        old.completed_unix_ms = old_time;
        let mut task = old.clone();
        task.completed = completed;
        task.completed_unix_ms = submitted_time;
        let patched = patch_record(
            record.clone(),
            1,
            ItemPatch {
                task: Some(task),
                ..Default::default()
            },
            20,
        )
        .unwrap();
        let task = patched.task.as_ref().unwrap();
        assert_eq!(task.completed, completed);
        assert_eq!(task.completed_unix_ms, expected_time);
        assert_eq!(task.checklist, record.task.as_ref().unwrap().checklist);
        assert_eq!(patched.schedule, record.schedule);
        assert_eq!(patched.links, record.links);
        assert_eq!(patched.notes, record.notes);
        assert_eq!(patched.created_unix_ms, 10);
        assert_eq!(patched.modified_unix_ms, 20);
        assert_eq!(patched.revision, 2);
    }
    let mut record = create_record("local:test".into(), item(), 10);
    let mut task = record.task.clone().unwrap();
    task.completed = true;
    record.capabilities.complete = false;
    let patch = ItemPatch {
        task: Some(task),
        ..Default::default()
    };
    assert!(patch_record(record.clone(), 1, patch.clone(), 20).is_err());
    assert!(patch_record(record.clone(), 2, ItemPatch::default(), 20).is_err());
    record.task = None;
    assert!(patch_record(record, 1, patch, 20).is_err());
}

#[tokio::test]
async fn rollback_failure_never_publishes_uncommitted_reads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agenda");
    let store = Store::open(root.clone(), None).await.unwrap();
    let original = store
        .create("create".into(), item())
        .await
        .unwrap()
        .item
        .unwrap();
    let before = original.clone();
    store
        .call(move |db| {
            futures::executor::block_on(async {
                db.conn.execute("BEGIN IMMEDIATE", ()).await?;
                let mut pending = before;
                pending.revision += 1;
                pending.title = "uncommitted change".into();
                db.conn
                    .execute(
                        "UPDATE items SET revision=?,payload=? WHERE id=?",
                        turso::params![
                            pending.revision as i64,
                            serde_json::to_vec(&pending)?,
                            pending.id.clone()
                        ],
                    )
                    .await?;
                db.conn
                    .execute("UPDATE store_meta SET revision=revision+1 WHERE id=1", ())
                    .await?;
                let receipt = MutationResult {
                    item: Some(pending),
                    catalog_revision: 2,
                    request_id: "uncertain".into(),
                    deleted_id: None,
                };
                db.conn
                    .execute(
                        "INSERT INTO operation_receipts VALUES(?,?,?)",
                        turso::params!["uncertain", "a".repeat(64), serde_json::to_vec(&receipt)?],
                    )
                    .await?;
                // Real SQL writes plus an actual engine rollback error fed into the same
                // recovery policy as a failed ROLLBACK. The transaction remains active.
                let rollback = db
                    .conn
                    .execute("ROLLBACK TO absent_savepoint", ())
                    .await
                    .map(|_| ())
                    .map_err(Into::into);
                assert!(rollback.is_err());
                assert!(!db.conn.is_autocommit()?);
                db.record_rollback_result(rollback);
                Ok(())
            })
        })
        .await
        .unwrap();
    let receipt = store.receipt("uncertain".into()).await;
    let read_item = store.get(original.id.clone()).await;
    let snapshot = store.snapshot().await;
    assert!(store.shutdown().await.is_err());
    assert!(
        receipt.is_err(),
        "uncommitted receipt escaped recovery fence: {receipt:?}"
    );
    assert!(
        read_item.is_err(),
        "uncommitted item escaped recovery fence"
    );
    assert!(
        snapshot.is_err(),
        "uncommitted catalog escaped recovery fence"
    );
    let recovered = Store::open(root, None).await.unwrap();
    assert_eq!(
        recovered.get(original.id).await.unwrap().unwrap().title,
        original.title
    );
    assert_eq!(recovered.snapshot().await.unwrap().catalog_revision, 1);
    assert!(
        recovered
            .receipt("uncertain".into())
            .await
            .unwrap()
            .is_none()
    );
    let committed = recovered
        .create("after-recovery".into(), item())
        .await
        .unwrap();
    recovered
        .call(|db| {
            futures::executor::block_on(async {
                // COMMIT error after the transaction ended must not hide durable results.
                assert!(db.finish(Ok(())).await.is_err());
                assert!(db.conn.is_autocommit()?);
                Ok(())
            })
        })
        .await
        .unwrap();
    assert_eq!(
        recovered.receipt("after-recovery".into()).await.unwrap(),
        Some(committed.clone())
    );
    let committed_item = committed.item.unwrap();
    assert_eq!(
        recovered.get(committed_item.id.clone()).await.unwrap(),
        Some(committed_item)
    );
    assert_eq!(recovered.snapshot().await.unwrap().catalog_revision, 2);
    assert!(recovered.shutdown().await.is_err());
}

#[test]
fn model_date_and_payload_validation() {
    assert!(
        Schedule {
            start_date: Some("2026-02-30".into()),
            ..Default::default()
        }
        .validate(false)
        .is_err()
    );
    assert!(
        Schedule {
            mode: ScheduleMode::Timed,
            start_unix_ms: Some(5),
            end_unix_ms: Some(5),
            timezone: Some("UTC".into()),
            ..Default::default()
        }
        .validate(false)
        .is_err()
    );
    assert!(
        Schedule {
            mode: ScheduleMode::Timed,
            start_unix_ms: Some(5),
            end_unix_ms: Some(5),
            timezone: Some("UTC".into()),
            ..Default::default()
        }
        .validate(true)
        .is_ok()
    );
    assert!(
        Schedule {
            start_date: Some("2026-10-27".into()),
            start_unix_ms: Some(5),
            ..Default::default()
        }
        .validate(false)
        .is_err()
    );
}
