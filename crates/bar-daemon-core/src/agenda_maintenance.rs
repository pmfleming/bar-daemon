//! Offline logical transfers only. Never connects to D-Bus or selects a default store.
use crate::activity::agenda_store::Store;
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

/// Explicit offline Agenda maintenance operations. Stop the daemon manually first.
#[derive(Debug, Subcommand)]
pub enum AgendaMaintenance {
    /// Export an existing private store to a new private logical backup (never overwrites).
    Export {
        #[arg(long, value_name = "ABS_PRIVATE_DIR")]
        store_dir: PathBuf,
        #[arg(long, value_name = "ABS_NEW_FILE")]
        output: PathBuf,
    },
    /// Restore a private logical backup into a new directory (never overwrites).
    Restore {
        #[arg(long, value_name = "ABS_NEW_DIR")]
        store_dir: PathBuf,
        #[arg(long, value_name = "ABS_EXPORT_FILE")]
        input: PathBuf,
    },
}

impl AgendaMaintenance {
    pub(crate) async fn run(self) -> Result<()> {
        self.prepare()?.run().await
    }

    /// Check filesystem policy before opening a store or creating a destination.
    fn prepare(self) -> Result<PreparedTransfer> {
        let (store_dir, transfer) = match self {
            Self::Export { store_dir, output } => {
                validate_export(&store_dir, &output)?;
                (store_dir, Transfer::Export(output))
            }
            Self::Restore { store_dir, input } => {
                validate_restore(&store_dir, &input)?;
                (store_dir, Transfer::Restore(input))
            }
        };
        Ok(PreparedTransfer {
            store_dir,
            transfer,
        })
    }
}

enum Transfer {
    Export(PathBuf),
    Restore(PathBuf),
}

struct PreparedTransfer {
    store_dir: PathBuf,
    transfer: Transfer,
}

impl PreparedTransfer {
    async fn run(self) -> Result<()> {
        let store = match &self.transfer {
            Transfer::Export(_) => Store::open_existing(self.store_dir).await?,
            Transfer::Restore(_) => {
                // Exclusive creation refuses even an empty initialized DB. Retain a failed
                // new destination for diagnosis; never remove or reuse user files.
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&self.store_dir)
                    .context(
                        "restore requires a nonexistent store directory; nothing is overwritten",
                    )?;
                Store::open(self.store_dir, None).await?
            }
        };
        let result = match self.transfer {
            Transfer::Export(output) => store.export(output).await,
            Transfer::Restore(input) => store.restore(input).await,
        };
        // Both modes share this completion boundary, including transfer errors.
        finish(&store, result).await
    }
}

fn validate_export(store_dir: &Path, output: &Path) -> Result<()> {
    explicit_path(store_dir)?;
    explicit_path(output)?;
    private_directory(store_dir)?;
    let database = fs::symlink_metadata(store_dir.join("agenda.db"))
        .context("export requires an existing agenda.db; no database was created")?;
    ensure!(
        database.is_file() && database.len() > 0,
        "export requires a nonempty regular agenda.db"
    );
    let parent = output.parent().context("output needs a parent directory")?;
    private_directory(parent)?;
    let resolved_output = parent
        .canonicalize()?
        .join(output.file_name().context("output needs a filename")?);
    ensure!(
        !resolved_output.starts_with(store_dir.canonicalize()?),
        "export output must be outside the source store directory"
    );
    absent(output).context("export never overwrites an existing output")
}

fn validate_restore(store_dir: &Path, input: &Path) -> Result<()> {
    explicit_path(store_dir)?;
    explicit_path(input)?;
    private_directory(input.parent().context("input needs a parent directory")?)?;
    let source = fs::symlink_metadata(input).context("read restore input metadata")?;
    ensure!(
        source.is_file()
            && source.uid() == rustix::process::geteuid().as_raw()
            && source.permissions().mode() & 0o077 == 0,
        "restore input must be a private, owned regular file (not a symlink)"
    );
    private_directory(
        store_dir
            .parent()
            .context("store needs a parent directory")?,
    )
}

fn explicit_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && !path.components().any(|part| part == Component::ParentDir),
        "maintenance requires explicit absolute paths without '..'"
    );
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == rustix::process::geteuid().as_raw()
            && meta.permissions().mode() & 0o077 == 0,
        "{} must be a private, owned directory (mode 0700, not a symlink)",
        path.display()
    );
    Ok(())
}

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("{} already exists", path.display()),
    }
}

async fn finish(store: &Store, operation: Result<()>) -> Result<()> {
    let shutdown = store
        .shutdown()
        .await
        .context("maintenance shutdown/checkpoint failed");
    match (operation, shutdown) {
        (Err(operation), Err(shutdown)) => Err(operation.context(format!("also: {shutdown:#}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::agenda_store::model::{ItemPatch, NewItem};
    use serde_json::{Value, json};

    async fn export(root: &Path, output: &Path) -> Result<()> {
        AgendaMaintenance::Export {
            store_dir: root.into(),
            output: output.into(),
        }
        .run()
        .await
    }
    async fn restore(root: &Path, input: &Path) -> Result<()> {
        AgendaMaintenance::Restore {
            store_dir: root.into(),
            input: input.into(),
        }
        .run()
        .await
    }
    /// Establish the same offline boundary as an operator before taking a backup.
    async fn offline_backup(store: Store, root: &Path, output: &Path) -> Vec<u8> {
        store.shutdown().await.unwrap();
        export(root, output).await.unwrap();
        fs::read(output).unwrap()
    }
    fn private_tempdir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        temp
    }
    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[tokio::test]
    async fn logical_round_trip_preserves_migration_sources_records_revisions_and_receipts() {
        let temp = private_tempdir();
        let root = temp.path().join("original");
        let legacy = temp.path().join("todos.json");
        let legacy_bytes = serde_json::to_vec(&json!([{
            "id": "legacy-id", "source_id": "historical-source", "title": "Original",
            "completed": true, "priority": 9, "due_unix_ms": null,
            "due_date": "2026-10-30", "created_unix_ms": 1234, "completed_unix_ms": 5678
        }]))
        .unwrap();
        write_private(&legacy, &legacy_bytes);
        let store = Store::open(root.clone(), Some(legacy.clone()))
            .await
            .unwrap();
        let new_item: NewItem = serde_json::from_value(json!({
            "kind": "task", "title": "Post-migration edit", "notes": "plain\r\n🦀",
            "links": [{"id":"link", "label":"reference", "url":"https://example.org"}],
            "task": {"priority":42, "checklist":[{"id":"step","text":"independent","checked":true}]}
        }))
        .unwrap();
        let created = store
            .create("create-request".into(), new_item)
            .await
            .unwrap();
        let id = created.item.as_ref().unwrap().id.clone();
        let patched = store
            .patch(
                "patch-request".into(),
                id,
                1,
                ItemPatch {
                    title: Some("Accepted revision two".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let deletion = store
            .delete("delete-request".into(), "legacy-id".into(), 1)
            .await
            .unwrap();
        let expected = store.snapshot().await.unwrap();
        let backup = temp.path().join("backup.json");
        let backup_bytes = offline_backup(store, &root, &backup).await;
        let restored_root = temp.path().join("restored");
        restore(&restored_root, &backup).await.unwrap();
        let restored = Store::open_existing(restored_root.clone()).await.unwrap();
        let actual = restored.snapshot().await.unwrap();
        assert_eq!(actual.items, expected.items);
        assert_eq!(actual.catalog_revision, expected.catalog_revision);
        for receipt in [created, patched, deletion] {
            assert_eq!(
                restored.receipt(receipt.request_id.clone()).await.unwrap(),
                Some(receipt)
            );
        }
        restored.shutdown().await.unwrap();
        let second = temp.path().join("second.json");
        export(&restored_root, &second).await.unwrap();
        // Entire logical export equality includes source capabilities and original migration bytes.
        assert_eq!(read_json(&backup), read_json(&second));
        assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        let migration_backup = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("legacy-todos-")
            })
            .unwrap();
        assert_eq!(fs::read(&migration_backup).unwrap(), legacy_bytes);
        assert_eq!(
            fs::metadata(migration_backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for path in [
            &backup,
            &second,
            &restored_root.join("agenda.db"),
            &restored_root.join("agenda.db.lock"),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(&restored_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn refuses_missing_empty_corrupt_unknown_and_active_stores_without_output() {
        let temp = private_tempdir();
        let root = temp.path().join("store");
        let output = temp.path().join("backup.json");
        assert!(export(&root, &output).await.is_err());
        assert!(!root.exists());
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(export(&root, &output).await.is_err());
        assert!(!root.join("agenda.db").exists());
        write_private(&root.join("agenda.db"), b"");
        assert!(export(&root, &output).await.is_err());
        write_private(&root.join("agenda.db"), b"not a database");
        assert!(export(&root, &output).await.is_err());
        assert_eq!(fs::read(root.join("agenda.db")).unwrap(), b"not a database");
        assert!(!output.exists());
        fs::remove_file(root.join("agenda.db")).unwrap();
        let uninitialized = rusqlite::Connection::open(root.join("agenda.db")).unwrap();
        uninitialized.execute_batch("VACUUM").unwrap();
        drop(uninitialized);
        let before = fs::read(root.join("agenda.db")).unwrap();
        assert!(!before.is_empty());
        assert!(export(&root, &output).await.is_err());
        assert_eq!(fs::read(root.join("agenda.db")).unwrap(), before);
        assert!(!output.exists());
        fs::remove_file(root.join("agenda.db")).unwrap();
        let store = Store::open(root.clone(), None).await.unwrap();
        assert!(export(&root, &output).await.is_err());
        assert!(!output.exists());
        store.shutdown().await.unwrap();
        // Compatible SQLite header but newer application schema must remain byte-identical.
        let database = root.join("agenda.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection.pragma_update(None, "user_version", 999).unwrap();
        drop(connection);
        let before = fs::read(&database).unwrap();
        assert!(export(&root, &output).await.is_err());
        assert_eq!(fs::read(&database).unwrap(), before);
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn never_overwrites_inputs_outputs_or_existing_restore_directories() {
        let temp = private_tempdir();
        let root = temp.path().join("store");
        let store = Store::open(root.clone(), None).await.unwrap();
        let item =
            serde_json::from_value(json!({"kind":"task", "title":"Do not overwrite"})).unwrap();
        store.create("keep".into(), item).await.unwrap();
        let backup = temp.path().join("backup.json");
        let before = offline_backup(store, &root, &backup).await;
        assert!(export(&root, &backup).await.is_err());
        assert_eq!(fs::read(&backup).unwrap(), before);
        let destination = temp.path().join("restored");
        restore(&destination, &backup).await.unwrap();
        let db_before = fs::read(destination.join("agenda.db")).unwrap();
        assert!(restore(&destination, &backup).await.is_err());
        assert_eq!(fs::read(destination.join("agenda.db")).unwrap(), db_before);
        let empty = temp.path().join("existing-empty-dir");
        fs::create_dir(&empty).unwrap();
        assert!(restore(&empty, &backup).await.is_err());
        let sentinel = temp.path().join("sentinel");
        write_private(&sentinel, b"untouched");
        assert!(restore(&sentinel, &backup).await.is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        let link = temp.path().join("link.json");
        std::os::unix::fs::symlink(&backup, &link).unwrap();
        assert!(export(&root, &link).await.is_err());
        assert!(
            restore(&temp.path().join("symlink-input"), &link)
                .await
                .is_err()
        );
        assert!(
            export(&root, &root.join("unsafe-backup.json"))
                .await
                .is_err()
        );
        assert!(!root.join("unsafe-backup.json").exists());
        assert_eq!(fs::read(&backup).unwrap(), before);
    }

    #[tokio::test]
    async fn unsafe_permissions_refuse_and_write_failure_still_releases_owner() {
        let temp = private_tempdir();
        let root = temp.path().join("store");
        let store = Store::open(root.clone(), None).await.unwrap();
        store.shutdown().await.unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            export(&root, &temp.path().join("unsafe.json"))
                .await
                .is_err()
        );
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let no_write = temp.path().join("no-write");
        fs::create_dir(&no_write).unwrap();
        fs::set_permissions(&no_write, fs::Permissions::from_mode(0o500)).unwrap();
        let result = export(&root, &no_write.join("backup.json")).await;
        fs::set_permissions(&no_write, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert!(!no_write.join("backup.json").exists());
        let store = Store::open_existing(root).await.unwrap();
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn completion_preserves_transfer_and_shutdown_errors() {
        let temp = private_tempdir();
        let store = Store::open(temp.path().join("success"), None)
            .await
            .unwrap();
        finish(&store, Ok(())).await.unwrap();
        let shutdown_error = finish(&store, Ok(())).await.unwrap_err();
        assert!(format!("{shutdown_error:#}").contains("maintenance shutdown/checkpoint failed"));

        let store = Store::open(temp.path().join("failure"), None)
            .await
            .unwrap();
        let operation_error = finish(&store, Err(anyhow::anyhow!("transfer failed")))
            .await
            .unwrap_err();
        assert_eq!(operation_error.to_string(), "transfer failed");
        let both = finish(&store, Err(anyhow::anyhow!("transfer failed")))
            .await
            .unwrap_err();
        let message = format!("{both:#}");
        assert!(message.contains("transfer failed"));
        assert!(message.contains("maintenance shutdown/checkpoint failed"));
        // The failed transfer still closed the store and released exclusive ownership.
        let reopened = Store::open_existing(temp.path().join("failure"))
            .await
            .unwrap();
        reopened.shutdown().await.unwrap();
    }

    #[test]
    fn committed_wal_child() {
        let Some(root) = std::env::var_os("AGENDA_MAINTENANCE_TEST_ROOT") else {
            return;
        };
        futures::executor::block_on(async {
            let store = Store::open(root.into(), None).await.unwrap();
            let item: NewItem = serde_json::from_value(json!({
                "kind": "event", "title": "Committed in WAL", "event": {"location":"Room", "people":["Person"]}
            })).unwrap();
            store.create("wal-receipt".into(), item).await.unwrap();
            // No drop, checkpoint or shutdown: the export must recover the committed WAL.
            std::process::exit(0);
        });
    }

    #[tokio::test]
    async fn export_recovers_actual_abrupt_exit_committed_wal() {
        let temp = private_tempdir();
        let root = temp.path().join("source");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "agenda_maintenance::tests::committed_wal_child"])
            .env("AGENDA_MAINTENANCE_TEST_ROOT", &root)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(fs::metadata(root.join("agenda.db-wal")).unwrap().len() > 0);
        let database_before = fs::read(root.join("agenda.db")).unwrap();
        let wal_before = fs::read(root.join("agenda.db-wal")).unwrap();
        let output = temp.path().join("wal-export.json");
        export(&root, &output).await.unwrap();
        assert_eq!(fs::read(root.join("agenda.db")).unwrap(), database_before);
        assert_eq!(fs::read(root.join("agenda.db-wal")).unwrap(), wal_before);
        let destination = temp.path().join("recovered");
        restore(&destination, &output).await.unwrap();
        let store = Store::open_existing(destination).await.unwrap();
        let receipt = store.receipt("wal-receipt".into()).await.unwrap().unwrap();
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.items, vec![receipt.item.unwrap()]);
        assert_eq!(snapshot.catalog_revision, 1);
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn restore_refuses_future_schema_and_invalid_records_atomically() {
        let temp = private_tempdir();
        let original = temp.path().join("original");
        let store = Store::open(original.clone(), None).await.unwrap();
        let item = serde_json::from_value(json!({"kind":"event", "title":"Valid"})).unwrap();
        store.create("receipt".into(), item).await.unwrap();
        let good = temp.path().join("good.json");
        let bytes = offline_backup(store, &original, &good).await;
        let base: Value = serde_json::from_slice(&bytes).unwrap();
        for kind in ["future-schema", "invalid-record"] {
            let mut payload = base.clone();
            if kind == "future-schema" {
                payload["schema"] = json!(999);
            } else {
                payload["snapshot"]["items"][0]["revision"] = json!(0);
            }
            let input = temp.path().join(format!("{kind}.json"));
            let bytes = serde_json::to_vec(&payload).unwrap();
            write_private(&input, &bytes);
            let destination = temp.path().join(kind);
            assert!(restore(&destination, &input).await.is_err());
            assert_eq!(fs::read(&input).unwrap(), bytes);
            let empty = Store::open_existing(destination).await.unwrap();
            let snapshot = empty.snapshot().await.unwrap();
            assert!(snapshot.items.is_empty());
            assert_eq!(snapshot.catalog_revision, 0);
            assert!(empty.receipt("receipt".into()).await.unwrap().is_none());
            empty.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn failed_restore_retains_source_and_releases_lock_without_retry_overwrite() {
        let temp = private_tempdir();
        let bad = temp.path().join("invalid.json");
        write_private(&bad, b"{invalid");
        let root = temp.path().join("failed");
        assert!(restore(&root, &bad).await.is_err());
        assert_eq!(fs::read(&bad).unwrap(), b"{invalid");
        let store = Store::open_existing(root.clone()).await.unwrap();
        assert!(store.snapshot().await.unwrap().items.is_empty());
        store.shutdown().await.unwrap();
        assert!(restore(&root, &bad).await.is_err());
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).unwrap();
        let refused = temp.path().join("not-created");
        assert!(restore(&refused, &bad).await.is_err());
        assert!(!refused.exists());
        assert!(export(Path::new("relative"), &bad).await.is_err());
        assert!(restore(Path::new("relative"), &bad).await.is_err());
    }
}
