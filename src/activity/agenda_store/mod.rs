//! Private embedded Turso owner. No connection escapes the bounded worker.
mod database;
mod migration;
pub(crate) mod model;
#[cfg(test)]
mod tests;

use anyhow::{Result, anyhow};
use database::Database;
use model::*;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(&mut Database) + Send>;
enum Command {
    Job(Job),
    Shutdown(oneshot::Sender<Result<()>>),
}
#[derive(Clone)]
pub(crate) struct Store {
    sender: Arc<mpsc::Sender<Command>>,
}
impl Store {
    pub async fn open(root: PathBuf, legacy_todo_path: Option<PathBuf>) -> Result<Self> {
        Self::open_mode(root, legacy_todo_path, false).await
    }
    /// Maintenance reader: never creates a directory/database or initializes a schema.
    pub async fn open_existing(root: PathBuf) -> Result<Self> {
        Self::open_mode(root, None, true).await
    }
    async fn open_mode(
        root: PathBuf,
        legacy_todo_path: Option<PathBuf>,
        existing_only: bool,
    ) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Command>(32);
        let (ready_tx, ready_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("agenda-database".into())
            .spawn(move || {
                let mut db = match futures::executor::block_on(Database::open(
                    &root,
                    legacy_todo_path.as_deref(),
                    existing_only,
                )) {
                    Ok(db) => {
                        let _ = ready_tx.send(Ok(()));
                        db
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let mut shutdowns = Vec::new();
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Job(job) => job(&mut db),
                        Command::Shutdown(reply) => {
                            receiver.close();
                            shutdowns.push(reply);
                        }
                    }
                }
                let result = futures::executor::block_on(db.checkpoint());
                let message = result.as_ref().err().map(ToString::to_string);
                // Release connection and ownership lock before acknowledging shutdown.
                drop(db);
                for reply in shutdowns {
                    let _ = reply.send(match &message {
                        Some(error) => Err(anyhow!(error.clone())),
                        None => Ok(()),
                    });
                }
                if let Some(error) = message {
                    tracing::error!(%error, "agenda shutdown checkpoint failed");
                }
            })?;
        ready_rx
            .await
            .map_err(|_| anyhow!("agenda worker startup failed"))??;
        Ok(Self {
            sender: Arc::new(sender),
        })
    }
    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Command::Job(Box::new(move |db| {
                let _ = tx.send(operation(db));
            })))
            .await
            .map_err(|_| anyhow!("agenda worker closed"))?;
        rx.await.map_err(|_| {
            anyhow!("agenda worker failed; resolve mutation by receipt after restart")
        })?
    }
    /// Fence accepted provider-content changes; never call for midnight/zone projection.
    pub async fn invalidate_catalog(&self) -> Result<u64> {
        self.call(|db| futures::executor::block_on(db.invalidate_catalog()))
            .await
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.call(|db| futures::executor::block_on(db.snapshot()))
            .await
    }
    pub async fn get(&self, id: String) -> Result<Option<Record>> {
        model::identifier(&id)?;
        self.call(move |db| futures::executor::block_on(db.get(&id)))
            .await
    }
    pub async fn receipt(&self, request_id: String) -> Result<Option<MutationResult>> {
        model::identifier(&request_id)?;
        self.call(move |db| {
            futures::executor::block_on(db.receipt(&request_id))
                .map(|r| r.map(|(_, result)| result))
        })
        .await
    }
    pub async fn create(&self, request_id: String, item: NewItem) -> Result<MutationResult> {
        model::identifier(&request_id)?;
        item.validate_bounds()?;
        self.call(move |db| {
            futures::executor::block_on(db.mutate(database::Mutation::Create { request_id, item }))
        })
        .await
    }
    pub async fn patch(
        &self,
        request_id: String,
        id: String,
        expected_revision: u64,
        patch: ItemPatch,
    ) -> Result<MutationResult> {
        model::identifier(&request_id)?;
        model::identifier(&id)?;
        patch.validate_bounds()?;
        self.call(move |db| {
            futures::executor::block_on(db.mutate(database::Mutation::Patch {
                request_id,
                id,
                expected_revision,
                patch,
            }))
        })
        .await
    }
    pub async fn delete(
        &self,
        request_id: String,
        id: String,
        expected_revision: u64,
    ) -> Result<MutationResult> {
        model::identifier(&request_id)?;
        model::identifier(&id)?;
        self.call(move |db| {
            futures::executor::block_on(db.mutate(database::Mutation::Delete {
                request_id,
                id,
                expected_revision,
            }))
        })
        .await
    }
    /// Consistent logical export includes receipts and sources, not a live DB-file copy.
    pub async fn export(&self, path: PathBuf) -> Result<()> {
        self.call(move |db| futures::executor::block_on(db.export(&path)))
            .await
    }
    /// Restore only into an empty initialized store. Existing records are never truncated.
    pub async fn restore(&self, path: PathBuf) -> Result<()> {
        self.call(move |db| futures::executor::block_on(db.restore(&path)))
            .await
    }
    pub async fn shutdown(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Command::Shutdown(tx))
            .await
            .map_err(|_| anyhow!("agenda worker closed"))?;
        rx.await
            .map_err(|_| anyhow!("agenda worker shutdown failed"))?
    }
}
