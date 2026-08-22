use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex, mpsc},
};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;

use super::model::{ActiveNotification, HistoryNotification};

const QUEUE_CAPACITY: usize = 1024;

#[derive(Debug)]
enum PersistenceCommand {
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
    List {
        before_history_id: Option<i64>,
        limit: usize,
        response: oneshot::Sender<Result<Vec<HistoryNotification>, String>>,
    },
}

#[derive(Clone)]
pub(crate) struct NotificationPersistence {
    commands: mpsc::SyncSender<PersistenceCommand>,
}

impl NotificationPersistence {
    pub(crate) fn open(path: &Path) -> Result<(Self, Vec<ActiveNotification>, bool, Option<u64>)> {
        let store = Arc::new(NotificationStore::open(path)?);
        let active = store.load_active()?;
        let (dnd, dnd_until_unix_ms) = store.load_dnd()?;
        let (commands, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let worker_store = Arc::clone(&store);
        std::thread::Builder::new()
            .name("notification-history".into())
            .spawn(move || persistence_worker(worker_store, receiver))
            .context("start notification persistence worker")?;
        Ok((Self { commands }, active, dnd, dnd_until_unix_ms))
    }

    pub(crate) fn save(&self, notification: ActiveNotification) {
        self.enqueue(PersistenceCommand::Save(Box::new(notification)));
    }

    pub(crate) fn close(&self, id: u32, closed_unix_ms: u64, reason: u32) {
        self.enqueue(PersistenceCommand::Close {
            id,
            closed_unix_ms,
            reason,
        });
    }

    pub(crate) fn clear(&self, closed_unix_ms: u64, reason: u32) {
        self.enqueue(PersistenceCommand::Clear {
            closed_unix_ms,
            reason,
        });
    }

    pub(crate) fn set_dnd(&self, enabled: bool, until_unix_ms: Option<u64>) {
        self.enqueue(PersistenceCommand::SetDnd {
            enabled,
            until_unix_ms,
        });
    }

    pub(crate) async fn list(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        let (response, receiver) = oneshot::channel();
        self.commands
            .try_send(PersistenceCommand::List {
                before_history_id,
                limit,
                response,
            })
            .context("notification persistence queue is full")?;
        receiver
            .await
            .context("notification persistence worker stopped")?
            .map_err(|error| anyhow!(error))
    }

    fn enqueue(&self, command: PersistenceCommand) {
        if let Err(error) = self.commands.try_send(command) {
            tracing::warn!(%error, "notification persistence queue is full");
        }
    }
}

fn persistence_worker(store: Arc<NotificationStore>, receiver: mpsc::Receiver<PersistenceCommand>) {
    while let Ok(command) = receiver.recv() {
        let result = match command {
            PersistenceCommand::Save(notification) => store.save(&notification),
            PersistenceCommand::Close {
                id,
                closed_unix_ms,
                reason,
            } => store.close(id, closed_unix_ms, reason),
            PersistenceCommand::Clear {
                closed_unix_ms,
                reason,
            } => store.clear(closed_unix_ms, reason),
            PersistenceCommand::SetDnd {
                enabled,
                until_unix_ms,
            } => store.set_dnd(enabled, until_unix_ms),
            PersistenceCommand::List {
                before_history_id,
                limit,
                response,
            } => {
                let result = store
                    .list(before_history_id, limit)
                    .map_err(|error| error.to_string());
                let _ = response.send(result);
                Ok(())
            }
        };
        if let Err(error) = result {
            tracing::warn!(%error, "notification history update failed");
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

    use super::NotificationStore;
    use crate::activity::notifications::model::{
        ActiveNotification, IncomingNotification, NotificationHints,
    };

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

    #[test]
    fn persists_active_and_closed_history() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let store = NotificationStore::open(&path).unwrap();
        store.save(&notification(7, false)).unwrap();
        store.close(7, 200, 2).unwrap();
        let history = store.list(None, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].notification.id, 7);
        assert_eq!(history[0].close_reason, Some(2));
    }

    #[test]
    fn does_not_retain_transient_notifications() {
        let directory = tempdir().unwrap();
        let store =
            NotificationStore::open(&directory.path().join("notifications.sqlite3")).unwrap();
        store.save(&notification(8, true)).unwrap();
        assert!(store.list(None, 10).unwrap().is_empty());
    }
}
