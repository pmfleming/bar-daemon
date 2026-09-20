use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{mpsc, oneshot};

use super::model::{ActiveNotification, HistoryNotification};

const QUEUE_CAPACITY: usize = 1024;

#[derive(Debug)]
enum Mutation {
    Save(Box<ActiveNotification>),
    Close {
        id: u32,
        closed_unix_ms: u64,
        reason: u32,
    },
    Clear {
        closed_unix_ms: u64,
        reason: u32,
    },
    SetDnd {
        enabled: bool,
        until_unix_ms: Option<u64>,
    },
}

#[derive(Debug)]
enum PersistenceCommand {
    Mutate(Vec<Mutation>),
    List {
        before_history_id: Option<i64>,
        limit: usize,
        response: oneshot::Sender<Result<Vec<HistoryNotification>, String>>,
    },
}

#[derive(Clone)]
pub(crate) struct NotificationPersistence {
    commands: mpsc::Sender<PersistenceCommand>,
}

impl NotificationPersistence {
    #[cfg(test)]
    pub(super) fn stopped_for_test() -> Self {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        Self { commands }
    }

    pub(crate) fn open(path: &Path) -> Result<(Self, Vec<ActiveNotification>, bool, Option<u64>)> {
        let store = Arc::new(NotificationStore::open(path)?);
        let active = store.load_active()?;
        let (dnd, dnd_until_unix_ms) = store.load_dnd()?;
        let (commands, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let worker_store = Arc::clone(&store);
        std::thread::Builder::new()
            .name("notification-history".into())
            .spawn(move || persistence_worker(worker_store, receiver))
            .context("start notification persistence worker")?;
        Ok((Self { commands }, active, dnd, dnd_until_unix_ms))
    }

    // Reserve before changing engine state. A full queue applies async
    // backpressure, and cancellation while waiting has no side effects.
    pub(crate) async fn reserve(&self) -> Result<PendingWrites<'_>> {
        Ok(PendingWrites {
            permit: Some(
                self.commands
                    .reserve()
                    .await
                    .context("notification persistence worker stopped")?,
            ),
            mutations: Vec::new(),
        })
    }

    pub(crate) async fn list(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        let (response, receiver) = oneshot::channel();
        self.commands
            .send(PersistenceCommand::List {
                before_history_id,
                limit,
                response,
            })
            .await
            .context("notification persistence worker stopped")?;
        receiver
            .await
            .context("notification persistence worker stopped")?
            .map_err(|error| anyhow!(error))
    }
}

// One reserved slot covers a whole engine mutation (including eviction or
// expiry). Drop enqueues synchronously, even if publication is cancelled after
// the in-memory change. The engine mutation lock preserves batch ordering.
pub(crate) struct PendingWrites<'a> {
    permit: Option<mpsc::Permit<'a, PersistenceCommand>>,
    mutations: Vec<Mutation>,
}

impl PendingWrites<'_> {
    pub(crate) fn save(&mut self, notification: ActiveNotification) {
        self.mutations.push(Mutation::Save(Box::new(notification)));
    }

    pub(crate) fn close(&mut self, id: u32, closed_unix_ms: u64, reason: u32) {
        self.mutations.push(Mutation::Close {
            id,
            closed_unix_ms,
            reason,
        });
    }

    pub(crate) fn clear(&mut self, closed_unix_ms: u64, reason: u32) {
        self.mutations.push(Mutation::Clear {
            closed_unix_ms,
            reason,
        });
    }

    pub(crate) fn set_dnd(&mut self, enabled: bool, until_unix_ms: Option<u64>) {
        self.mutations.push(Mutation::SetDnd {
            enabled,
            until_unix_ms,
        });
    }
}

impl Drop for PendingWrites<'_> {
    fn drop(&mut self) {
        if !self.mutations.is_empty()
            && let Some(permit) = self.permit.take()
        {
            permit.send(PersistenceCommand::Mutate(std::mem::take(
                &mut self.mutations,
            )));
        }
    }
}

fn persistence_worker(
    store: Arc<NotificationStore>,
    mut receiver: mpsc::Receiver<PersistenceCommand>,
) {
    while let Some(command) = receiver.blocking_recv() {
        match command {
            PersistenceCommand::Mutate(mutations) => {
                for mutation in mutations {
                    let result = match mutation {
                        Mutation::Save(notification) => store.save(&notification),
                        Mutation::Close {
                            id,
                            closed_unix_ms,
                            reason,
                        } => store.close(id, closed_unix_ms, reason),
                        Mutation::Clear {
                            closed_unix_ms,
                            reason,
                        } => store.clear(closed_unix_ms, reason),
                        Mutation::SetDnd {
                            enabled,
                            until_unix_ms,
                        } => store.set_dnd(enabled, until_unix_ms),
                    };
                    if let Err(error) = result {
                        tracing::warn!(%error, "notification history update failed");
                    }
                }
            }
            PersistenceCommand::List {
                before_history_id,
                limit,
                response,
            } => {
                let result = store
                    .list(before_history_id, limit)
                    .map_err(|error| error.to_string());
                let _ = response.send(result);
            }
        }
    }
}

struct NotificationStore {
    connection: Mutex<Connection>,
}

impl NotificationStore {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create notification state directory {}", parent.display())
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open notification database {}", path.display()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS notifications (
                   history_id INTEGER PRIMARY KEY AUTOINCREMENT,
                   session_id INTEGER NOT NULL,
                   payload_json TEXT NOT NULL,
                   created_unix_ms INTEGER NOT NULL,
                   updated_unix_ms INTEGER NOT NULL,
                   closed_unix_ms INTEGER,
                   close_reason INTEGER
                 );
                 CREATE INDEX IF NOT EXISTS notifications_active_session
                   ON notifications(session_id) WHERE closed_unix_ms IS NULL;
                 CREATE INDEX IF NOT EXISTS notifications_history_order
                   ON notifications(history_id DESC);
                 CREATE TABLE IF NOT EXISTS notification_meta (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL
                 );",
            )
            .context("initialize notification database")?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("notification database lock poisoned"))
    }

    fn load_active(&self) -> Result<Vec<ActiveNotification>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM notifications
             WHERE closed_unix_ms IS NULL ORDER BY history_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut active = Vec::new();
        for payload in rows {
            let payload = payload?;
            match serde_json::from_str(&payload) {
                Ok(notification) => active.push(notification),
                Err(error) => tracing::warn!(%error, "ignored invalid persisted notification"),
            }
        }
        Ok(active)
    }

    fn load_dnd(&self) -> Result<(bool, Option<u64>)> {
        let connection = self.connection()?;
        let value = |key: &str| -> Result<Option<String>> {
            Ok(connection
                .query_row(
                    "SELECT value FROM notification_meta WHERE key = ?1",
                    [key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        };
        let enabled = value("dnd")?.as_deref() == Some("true");
        let until = value("dnd_until_unix_ms")?.and_then(|stored| stored.parse::<u64>().ok());
        Ok((enabled, enabled.then_some(until).flatten()))
    }

    fn save(&self, notification: &ActiveNotification) -> Result<()> {
        let connection = self.connection()?;
        if notification.hints.transient {
            connection.execute(
                "DELETE FROM notifications WHERE session_id = ?1 AND closed_unix_ms IS NULL",
                [notification.id],
            )?;
            return Ok(());
        }
        let payload = serde_json::to_string(notification)?;
        let changed = connection.execute(
            "UPDATE notifications
             SET payload_json = ?2, updated_unix_ms = ?3
             WHERE session_id = ?1 AND closed_unix_ms IS NULL",
            params![notification.id, payload, notification.updated_unix_ms],
        )?;
        if changed == 0 {
            connection.execute(
                "INSERT INTO notifications
                 (session_id, payload_json, created_unix_ms, updated_unix_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    notification.id,
                    payload,
                    notification.created_unix_ms,
                    notification.updated_unix_ms
                ],
            )?;
        }
        Ok(())
    }

    fn close(&self, id: u32, closed_unix_ms: u64, reason: u32) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE notifications SET closed_unix_ms = ?2, close_reason = ?3
             WHERE session_id = ?1 AND closed_unix_ms IS NULL",
            params![id, closed_unix_ms, reason],
        )?;
        Ok(())
    }

    fn clear(&self, closed_unix_ms: u64, reason: u32) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE notifications SET closed_unix_ms = ?1, close_reason = ?2
             WHERE closed_unix_ms IS NULL",
            params![closed_unix_ms, reason],
        )?;
        Ok(())
    }

    fn set_dnd(&self, enabled: bool, until_unix_ms: Option<u64>) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO notification_meta(key, value) VALUES ('dnd', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [if enabled { "true" } else { "false" }],
        )?;
        if let Some(until) = enabled.then_some(until_unix_ms).flatten() {
            transaction.execute(
                "INSERT INTO notification_meta(key, value) VALUES ('dnd_until_unix_ms', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [until.to_string()],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM notification_meta WHERE key = 'dnd_until_unix_ms'",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn list(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        let connection = self.connection()?;
        let before = before_history_id.unwrap_or(i64::MAX);
        let mut statement = connection.prepare(
            "SELECT history_id, payload_json, closed_unix_ms, close_reason
             FROM notifications WHERE history_id < ?1
             ORDER BY history_id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![before, limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
                row.get::<_, Option<u32>>(3)?,
            ))
        })?;
        let mut history = Vec::new();
        for row in rows {
            let (history_id, payload, closed_unix_ms, close_reason) = row?;
            let notification =
                serde_json::from_str(&payload).context("decode persisted notification history")?;
            history.push(HistoryNotification {
                history_id,
                notification,
                closed_unix_ms,
                close_reason,
            });
        }
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{NotificationPersistence, NotificationStore, persistence_worker};
    use crate::activity::notifications::model::{
        ActiveNotification, IncomingNotification, NotificationHints,
    };
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn notification(id: u32, transient: bool) -> ActiveNotification {
        ActiveNotification::from_incoming(
            id,
            IncomingNotification {
                app_name: "test".into(),
                app_icon: String::new(),
                summary: format!("notification {id}"),
                body: String::new(),
                actions: Vec::new(),
                hints: NotificationHints {
                    transient,
                    ..NotificationHints::default()
                },
                expire_timeout: 0,
            },
            100,
        )
    }

    #[tokio::test]
    async fn saturated_queue_preserves_ordered_mutations_and_restart_state() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let store = Arc::new(NotificationStore::open(&path).unwrap());
        let (commands, receiver) = mpsc::channel(1);
        let persistence = NotificationPersistence { commands };
        persistence
            .reserve()
            .await
            .unwrap()
            .save(notification(1, false));

        // No worker is running: reservation must wait rather than lose a write.
        let worker = {
            let reserve = persistence.reserve();
            tokio::pin!(reserve);
            assert!(futures::poll!(&mut reserve).is_pending());
            let worker = std::thread::spawn(move || persistence_worker(store, receiver));
            let mut writes = reserve.await.unwrap();
            writes.close(1, 200, 2);
            writes.save(notification(2, false));
            writes.save(notification(3, true)); // transient content must not survive restart
            writes.set_dnd(true, Some(1234));
            worker
        };
        let history = persistence.list(None, 10).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].notification.id, 2);
        assert_eq!(history[1].close_reason, Some(2));
        assert_eq!(
            NotificationStore::open(&path)
                .unwrap()
                .load_active()
                .unwrap()
                .len(),
            1
        );

        persistence.reserve().await.unwrap().clear(300, 2);
        let history = persistence.list(None, 10).await.unwrap();
        assert!(history.iter().all(|item| item.closed_unix_ms.is_some()));
        drop(persistence);
        worker.join().unwrap();
        let restarted = NotificationStore::open(&path).unwrap();
        assert!(restarted.load_active().unwrap().is_empty());
        assert_eq!(restarted.load_dnd().unwrap(), (true, Some(1234)));
    }

    #[tokio::test]
    async fn cancelled_reservations_and_mutations_do_not_lose_accepted_writes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let store = Arc::new(NotificationStore::open(&path).unwrap());
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
            // Dropping a waiter must leave the accepted save untouched.
        }
        let worker = std::thread::spawn(move || persistence_worker(store, receiver));
        let (changed, received) = tokio::sync::oneshot::channel();
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
        drop(persistence);
        worker.join().unwrap();
        let restarted = NotificationStore::open(&path).unwrap();
        assert!(restarted.load_active().unwrap().is_empty());
        assert_eq!(restarted.load_dnd().unwrap(), (true, None));
    }
}
