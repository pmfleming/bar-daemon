use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{mpsc, oneshot};

use super::{
    history::{CatalogRecord, PAGE_BYTES, Position, SCOPE_LIMIT, search_text},
    model::{ActiveNotification, HistoryNotification},
};

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
    Query {
        query: String,
        before: Option<Position>,
        limit: usize,
        excluded: Vec<(u64, u32)>,
        response: oneshot::Sender<Result<Vec<CatalogRecord>>>,
    },
    List {
        before_history_id: Option<i64>,
        limit: usize,
        response: oneshot::Sender<Result<Vec<HistoryNotification>>>,
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

    pub(crate) fn open(path: &Path) -> Result<(Self, u32, bool, Option<u64>)> {
        let store = NotificationStore::open(path)?;
        let last_id = store.last_id()?;
        // Persisted content is history, not a surviving D-Bus conversation.
        // Do not replay old popups/actions after the server session restarts.
        store.clear(
            crate::time::unix_ms(),
            super::model::close_reason::UNDEFINED,
        )?;
        let (dnd, dnd_until_unix_ms) = store.load_dnd()?;
        let (commands, receiver) = mpsc::channel(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("notification-history".into())
            .spawn(move || persistence_worker(store, receiver))
            .context("start notification persistence worker")?;
        Ok((Self { commands }, last_id, dnd, dnd_until_unix_ms))
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

    pub(crate) async fn query(
        &self,
        query: String,
        before: Option<Position>,
        limit: usize,
        excluded: Vec<(u64, u32)>,
    ) -> Result<Vec<CatalogRecord>> {
        self.request(|response| PersistenceCommand::Query {
            query,
            before,
            limit,
            excluded,
            response,
        })
        .await
    }

    pub(crate) async fn list(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        self.request(|response| PersistenceCommand::List {
            before_history_id,
            limit,
            response,
        })
        .await
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T>>) -> PersistenceCommand,
    ) -> Result<T> {
        let (response, receiver) = oneshot::channel();
        self.commands
            .send(command(response))
            .await
            .context("notification persistence worker stopped")?;
        receiver
            .await
            .context("notification persistence worker stopped")?
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
    mut store: NotificationStore,
    mut receiver: mpsc::Receiver<PersistenceCommand>,
) {
    let mut history_error = None;
    while let Some(command) = receiver.blocking_recv() {
        match command {
            PersistenceCommand::Mutate(mutations) => {
                apply_mutations(&mut store, mutations, &mut history_error);
            }
            PersistenceCommand::Query {
                query,
                before,
                limit,
                excluded,
                response,
            } => reply(response, history_error.as_deref(), || {
                store.query(&query, before, limit, &excluded)
            }),
            // The legacy list has no catalog revision: retain its best-effort
            // history semantics, but skip reads whose caller has gone away.
            PersistenceCommand::List {
                before_history_id,
                limit,
                response,
            } => reply(response, None, || store.list(before_history_id, limit)),
        }
    }
}

fn apply_mutations(
    store: &mut NotificationStore,
    mutations: Vec<Mutation>,
    history_error: &mut Option<String>,
) {
    for mutation in mutations {
        if let Err(error) = store.apply(mutation) {
            tracing::warn!(%error, "notification history update failed");
            // Continue accepted writes, but never label stale storage with a
            // newer engine revision, even when later writes succeed.
            *history_error = Some(error.to_string());
        }
    }
}

fn reply<T>(
    response: oneshot::Sender<Result<T>>,
    history_error: Option<&str>,
    read: impl FnOnce() -> Result<T>,
) {
    if response.is_closed() {
        return;
    }
    let result = match history_error {
        Some(error) => Err(anyhow!("{error}")),
        None => read(),
    };
    let _ = response.send(result);
}

struct NotificationStore {
    connection: Connection,
}

impl NotificationStore {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create notification state directory {}", parent.display())
            })?;
        }
        let mut connection = Connection::open(path)
            .with_context(|| format!("open notification database {}", path.display()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS notifications (
                   history_id INTEGER PRIMARY KEY AUTOINCREMENT,
                   session_id INTEGER NOT NULL,
                   payload_json TEXT NOT NULL,
                   search_text TEXT NOT NULL DEFAULT '',
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
        let has_search = connection
            .prepare("PRAGMA table_info(notifications)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == "search_text");
        {
            // Legacy writers omit the search column. An invalidation trigger
            // and dirty-row index make rollback/re-upgrade safe without scanning
            // or rewriting all retained payloads on every native startup.
            let transaction = connection.transaction()?;
            if !has_search {
                transaction.execute(
                    "ALTER TABLE notifications ADD COLUMN search_text TEXT NOT NULL DEFAULT ''",
                    [],
                )?;
            }
            transaction.execute_batch(
                "CREATE TRIGGER IF NOT EXISTS notifications_search_invalidate
                   AFTER UPDATE OF payload_json ON notifications BEGIN
                     UPDATE notifications SET search_text = '' WHERE history_id = NEW.history_id;
                   END;
                 CREATE INDEX IF NOT EXISTS notifications_search_dirty ON notifications(history_id) WHERE search_text = '';"
            )?;
            let mut after = 0;
            loop {
                let rows = transaction.prepare(
                    "SELECT history_id, payload_json FROM notifications WHERE search_text = '' AND history_id > ?1 ORDER BY history_id LIMIT 128")?
                    .query_map([after], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if rows.is_empty() {
                    break;
                }
                for (id, payload) in rows {
                    let notification: ActiveNotification = serde_json::from_str(&payload)?;
                    transaction.execute(
                        "UPDATE notifications SET search_text = ?2 WHERE history_id = ?1",
                        params![id, search_text(&notification)],
                    )?;
                    after = id;
                }
            }
            transaction.commit()?;
        }
        Ok(Self { connection })
    }

    fn apply(&mut self, mutation: Mutation) -> Result<()> {
        match mutation {
            Mutation::Save(notification) => self.save(&notification),
            Mutation::Close {
                id,
                closed_unix_ms,
                reason,
            } => self.close(id, closed_unix_ms, reason),
            Mutation::Clear {
                closed_unix_ms,
                reason,
            } => self.clear(closed_unix_ms, reason),
            Mutation::SetDnd {
                enabled,
                until_unix_ms,
            } => self.set_dnd(enabled, until_unix_ms),
        }
    }

    fn last_id(&self) -> Result<u32> {
        // Include closed legacy rows on migration and transient IDs (metadata).
        Ok(self.connection.query_row(
            "SELECT MAX(id) FROM (SELECT COALESCE(MAX(session_id), 0) AS id FROM notifications
             UNION ALL SELECT CAST(value AS INTEGER) FROM notification_meta WHERE key = 'last_notification_id')",
            [],
            |row| row.get(0),
        )?)
    }

    fn load_dnd(&self) -> Result<(bool, Option<u64>)> {
        let connection = &self.connection;
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
        self.connection.execute(
            "INSERT INTO notification_meta (key, value) VALUES ('last_notification_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = MAX(CAST(value AS INTEGER), CAST(excluded.value AS INTEGER))",
            [notification.id],
        )?;
        if notification.hints.transient {
            self.connection.execute(
                "DELETE FROM notifications WHERE session_id = ?1 AND closed_unix_ms IS NULL",
                [notification.id],
            )?;
            return Ok(());
        }
        let payload = serde_json::to_string(notification)?;
        let changed = self.connection.execute(
            "UPDATE notifications
             SET payload_json = ?2, updated_unix_ms = ?3
             WHERE session_id = ?1 AND closed_unix_ms IS NULL",
            params![notification.id, payload, notification.updated_unix_ms],
        )?;
        if changed == 0 {
            self.connection.execute(
                "INSERT INTO notifications
                 (session_id, payload_json, created_unix_ms, updated_unix_ms, search_text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    notification.id,
                    payload,
                    notification.created_unix_ms,
                    notification.updated_unix_ms,
                    search_text(notification)
                ],
            )?;
        } else {
            self.connection.execute(
                "UPDATE notifications SET search_text = ?2 WHERE session_id = ?1 AND closed_unix_ms IS NULL",
                params![notification.id, search_text(notification)],
            )?;
        }
        Ok(())
    }

    fn close(&self, id: u32, closed_unix_ms: u64, reason: u32) -> Result<()> {
        self.connection.execute(
            "UPDATE notifications SET closed_unix_ms = ?2, close_reason = ?3
             WHERE session_id = ?1 AND closed_unix_ms IS NULL",
            params![id, closed_unix_ms, reason],
        )?;
        Ok(())
    }

    fn clear(&self, closed_unix_ms: u64, reason: u32) -> Result<()> {
        self.connection.execute(
            "UPDATE notifications SET closed_unix_ms = ?1, close_reason = ?2
             WHERE closed_unix_ms IS NULL",
            params![closed_unix_ms, reason],
        )?;
        Ok(())
    }

    fn set_dnd(&mut self, enabled: bool, until_unix_ms: Option<u64>) -> Result<()> {
        let transaction = self.connection.transaction()?;
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

    fn query(
        &self,
        query: &str,
        before: Option<Position>,
        limit: usize,
        excluded: &[(u64, u32)],
    ) -> Result<Vec<CatalogRecord>> {
        let position = before.unwrap_or(Position {
            created: i64::MAX as u64,
            id: u32::MAX,
        });
        let mut statement = self.connection.prepare(
            "WITH matched AS MATERIALIZED (
               SELECT history_id, created_unix_ms, session_id FROM notifications
               WHERE history_id IN (SELECT history_id FROM notifications ORDER BY history_id DESC LIMIT ?1)
                 AND (created_unix_ms < ?2 OR (created_unix_ms = ?2 AND session_id < ?3))
                 AND instr(search_text, ?4) > 0
               ORDER BY created_unix_ms DESC, session_id DESC LIMIT ?5)
             SELECT n.history_id, n.payload_json, n.closed_unix_ms, n.close_reason
             FROM matched JOIN notifications n USING(history_id)
             ORDER BY matched.created_unix_ms DESC, matched.session_id DESC")?;
        let mut rows = statement.query(params![
            SCOPE_LIMIT,
            position.created,
            position.id,
            query,
            limit + 1 + excluded.len()
        ])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            anyhow::ensure!(
                row.get_ref(1)?.as_str()?.len() <= PAGE_BYTES,
                "Notification exceeds history page byte limit"
            );
            let record = history_record(row)?;
            if excluded.contains(&(record.notification.created_unix_ms, record.notification.id)) {
                continue;
            }
            records.push(CatalogRecord {
                history_id: Some(record.history_id),
                notification: record.notification,
                closed_unix_ms: record.closed_unix_ms,
                close_reason: record.close_reason,
            });
            if records.len() > limit {
                break;
            }
        }
        Ok(records)
    }

    fn list(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        let before = before_history_id.unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT history_id, payload_json, closed_unix_ms, close_reason
             FROM notifications WHERE history_id < ?1
             ORDER BY history_id DESC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![before, limit])?;
        let mut history = Vec::new();
        while let Some(row) = rows.next()? {
            history.push(history_record(row).context("decode persisted notification history")?);
        }
        Ok(history)
    }
}

// Deserialize directly from SQLite's row buffer; both history APIs share the
// same schema without allocating an intermediate payload String.
fn history_record(row: &rusqlite::Row<'_>) -> Result<HistoryNotification> {
    Ok(HistoryNotification {
        history_id: row.get(0)?,
        notification: serde_json::from_str(row.get_ref(1)?.as_str()?)?,
        closed_unix_ms: row.get(2)?,
        close_reason: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{NotificationPersistence, NotificationStore, persistence_worker, reply};
    use crate::activity::notifications::model::{
        ActiveNotification, IncomingNotification, NotificationHints,
    };
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn cancelled_and_fenced_reads_do_not_execute_storage_work() {
        let calls = std::cell::Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        for error in [None, Some("write failed")] {
            let (response, receiver) = oneshot::channel();
            drop(receiver);
            reply(response, error, read);
        }
        let (response, receiver) = oneshot::channel();
        reply(response, Some("write failed"), read);
        assert_eq!(
            receiver.await.unwrap().unwrap_err().to_string(),
            "write failed"
        );
        assert_eq!(calls.get(), 0);
        let (response, receiver) = oneshot::channel();
        reply(response, None, read);
        assert_eq!(receiver.await.unwrap().unwrap(), 7);
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn failed_write_fences_queries_but_does_not_discard_later_mutations() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let store = NotificationStore::open(&path).unwrap();
        store.save(&notification(1, false)).unwrap();
        store.connection.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON notifications BEGIN SELECT RAISE(FAIL, 'simulated disk failure'); END;").unwrap();
        let (commands, receiver) = mpsc::channel(1);
        let persistence = NotificationPersistence { commands };
        let worker = std::thread::spawn(move || persistence_worker(store, receiver));
        {
            let mut writes = persistence.reserve().await.unwrap();
            writes.save(notification(1, true)); // Fails to remove the persisted copy.
            writes.save(notification(2, false));
            writes.set_dnd(true, None);
        }
        assert_eq!(persistence.list(None, 10).await.unwrap().len(), 2);
        let first = persistence
            .query(String::new(), None, 10, Vec::new())
            .await
            .unwrap_err();
        assert!(first.to_string().contains("simulated disk failure"));
        persistence.reserve().await.unwrap().close(2, 200, 2);
        let second = persistence
            .query(String::new(), None, 10, Vec::new())
            .await
            .unwrap_err();
        assert_eq!(second.to_string(), first.to_string());
        assert_eq!(
            persistence.list(None, 10).await.unwrap()[0].close_reason,
            Some(2)
        );
        drop(persistence);
        worker.join().unwrap();
        assert_eq!(
            NotificationStore::open(&path).unwrap().load_dnd().unwrap(),
            (true, None)
        );
    }

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
        let store = NotificationStore::open(&path).unwrap();
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
                .list(None, 10)
                .unwrap()
                .iter()
                .filter(|item| item.closed_unix_ms.is_none())
                .count(),
            1
        );

        persistence.reserve().await.unwrap().clear(300, 2);
        let history = persistence.list(None, 10).await.unwrap();
        assert!(history.iter().all(|item| item.closed_unix_ms.is_some()));
        drop(persistence);
        worker.join().unwrap();
        let restarted = NotificationStore::open(&path).unwrap();
        assert!(
            restarted
                .list(None, 10)
                .unwrap()
                .iter()
                .all(|item| item.closed_unix_ms.is_some())
        );
        assert_eq!(restarted.load_dnd().unwrap(), (true, Some(1234)));
    }

    #[tokio::test]
    async fn cancelled_reservations_and_mutations_do_not_lose_accepted_writes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let store = NotificationStore::open(&path).unwrap();
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
        assert!(
            restarted
                .list(None, 10)
                .unwrap()
                .iter()
                .all(|item| item.closed_unix_ms.is_some())
        );
        assert_eq!(restarted.load_dnd().unwrap(), (true, None));
    }
}
