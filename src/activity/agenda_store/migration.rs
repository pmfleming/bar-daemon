//! One-way JSON adoption. Original bytes remain in a private backup and durable receipt.
use super::{
    database::{Database, fingerprint, private_file},
    model::*,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::Path,
};
use turso::params;

// Match actual activity::model::TodoItem serialization, not a guessed wrapper.
#[derive(Debug, Deserialize, Serialize)]
struct LegacyTodo {
    id: String,
    source_id: String,
    title: String,
    completed: bool,
    priority: u8,
    due_unix_ms: Option<i64>,
    due_date: Option<String>,
    created_unix_ms: i64,
    completed_unix_ms: Option<i64>,
}
impl Database {
    pub(super) async fn migrate_legacy(&mut self, root: &Path, path: &Path) -> Result<()> {
        let bytes = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).context("read legacy todo store; writable mode blocked");
            }
        };
        let hash = bytes
            .as_ref()
            .map(|bytes| fingerprint(bytes))
            .unwrap_or_else(|| "missing".into());
        let previous = self
            .conn
            .query(
                "SELECT fingerprint FROM migrations WHERE name='legacy-todos-v1'",
                (),
            )
            .await?
            .next()
            .await?;
        if let Some(row) = previous {
            let previous: String = row.get(0)?;
            // Moving the original away is supported; changed content is not a second import.
            ensure!(
                bytes.is_none() || previous == hash,
                "legacy todo content changed after migration; explicit reconciliation required"
            );
            return Ok(());
        }
        ensure!(
            bytes.as_ref().is_none_or(|b| b.len() <= 32 * 1024 * 1024),
            "legacy todo file too large"
        );
        let todos: Vec<LegacyTodo> = match &bytes {
            Some(bytes) => serde_json::from_slice(bytes)
                .context("parse legacy todo store; original preserved")?,
            None => Vec::new(),
        };
        ensure!(todos.len() <= 10000, "legacy todo capacity exceeded");
        let mut ids = HashSet::new();
        let mut records = Vec::new();
        for todo in todos {
            ensure!(ids.insert(todo.id.clone()), "duplicate legacy todo id");
            let schedule = if todo.due_date.is_some() {
                // Preserve both raw fields in the receipt when historical clients sent both.
                Schedule {
                    end_date: todo.due_date,
                    ..Default::default()
                }
            } else if todo.due_unix_ms.is_some() {
                Schedule {
                    mode: ScheduleMode::Timed,
                    end_unix_ms: todo.due_unix_ms,
                    timezone: Some("UTC".into()),
                    ..Default::default()
                }
            } else {
                Schedule::default()
            };
            let record = Record {
                id: todo.id,
                kind: ItemKind::Task,
                source_id: todo.source_id,
                source_uid: None,
                revision: 1,
                title: todo.title,
                notes: String::new(),
                links: Vec::new(),
                schedule,
                created_unix_ms: todo.created_unix_ms,
                modified_unix_ms: todo.completed_unix_ms.unwrap_or(todo.created_unix_ms),
                capabilities: Capabilities::local(),
                event: None,
                task: Some(TaskDetails {
                    priority: i64::from(todo.priority),
                    completed: todo.completed,
                    completed_unix_ms: todo.completed_unix_ms,
                    ..Default::default()
                }),
            };
            record
                .validate()
                .context("invalid legacy todo; migration blocked without dropping data")?;
            records.push(record);
        }
        if let Some(bytes) = &bytes {
            let backup = root.join(format!("legacy-todos-{hash}.json"));
            if backup.try_exists()? {
                ensure!(fs::read(&backup)? == *bytes, "legacy backup mismatch");
                drop(private_file(&backup, false)?);
            } else {
                let mut file = private_file(&backup, true)?;
                file.write_all(bytes)?;
                file.sync_all()?;
                File::open(root)?.sync_all()?;
            }
        }
        self.conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            for record in &records {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO sources VALUES(?,1,1,1)",
                        [record.source_id.as_str()],
                    )
                    .await?;
                self.insert_record(record).await?;
                ensure!(
                    self.get(&record.id).await?.as_ref() == Some(record),
                    "legacy import verification mismatch"
                );
                #[cfg(test)]
                if std::env::var_os("AGENDA_TEST_INTERRUPT_MIGRATION").is_some() {
                    // Deterministic subprocess fault point: real SQL write, no
                    // receipt/COMMIT and no connection/worker destructors.
                    std::process::exit(0);
                }
            }
            if !records.is_empty() {
                self.conn
                    .execute("UPDATE store_meta SET revision=revision+1 WHERE id=1", ())
                    .await?;
            }
            self.conn
                .execute(
                    "INSERT INTO migrations VALUES('legacy-todos-v1',?,?)",
                    params![hash, bytes.unwrap_or_default()],
                )
                .await?;
            self.ensure_payload_budget().await?;
            Ok(())
        }
        .await;
        self.finish(result).await
    }
}
