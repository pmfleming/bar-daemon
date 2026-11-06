use std::{path::Path, time::Duration};

use anyhow::{Context, Result, anyhow};
use futures::executor::block_on;
use tokio::sync::{mpsc, oneshot};

use super::{
    center::{CenterPage, CenterQuery},
    database::NotificationStore,
    history::{CatalogRecord, Position},
    model::{ActiveNotification, HistoryNotification},
};

const QUEUE_CAPACITY: usize = 1024;

#[derive(Debug)]
pub(super) enum Mutation {
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
    Settings {
        value: Option<super::settings::Settings>,
        response: oneshot::Sender<Result<super::settings::Settings>>,
    },
    Prune {
        response: oneshot::Sender<Result<usize>>,
    },
    Once {
        key: String,
        notification: Box<ActiveNotification>,
        response: oneshot::Sender<Result<bool>>,
    },
    Mutate(Vec<Mutation>),
    Delete {
        positions: Vec<Position>,
        response: oneshot::Sender<Result<()>>,
    },
    Targets {
        app_key: Option<String>,
        selected: Option<Position>,
        response: oneshot::Sender<Result<Vec<Position>>>,
    },
    Policy {
        key: String,
        policy: super::policy::AppPolicy,
        response: oneshot::Sender<Result<()>>,
    },
    Policies {
        response:
            oneshot::Sender<Result<std::collections::BTreeMap<String, super::policy::AppPolicy>>>,
    },
    Center {
        query: Box<CenterQuery>,
        active: Vec<ActiveNotification>,
        epoch: String,
        revision: u64,
        response: oneshot::Sender<Result<CenterPage>>,
    },
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
    Shutdown {
        response: oneshot::Sender<Result<()>>,
    },
    #[cfg(test)]
    Fault {
        fault: super::database::Fault,
        response: oneshot::Sender<Result<()>>,
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

    pub(crate) async fn open(path: &Path) -> Result<(Self, u32, bool, Option<u64>)> {
        let path = path.to_owned();
        let (commands, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("notification-history".into())
            .spawn(move || {
                let opened = block_on(async {
                    let mut store = NotificationStore::open(&path).await?;
                    let last_id = store.last_id().await?;
                    // Stored content is history, never a surviving D-Bus conversation.
                    store
                        .apply(Mutation::Clear {
                            closed_unix_ms: crate::time::unix_ms(),
                            reason: super::model::close_reason::UNDEFINED,
                        })
                        .await?;
                    store.prune().await?;
                    let (dnd, until) = store.load_dnd().await?;
                    Ok::<_, anyhow::Error>((store, last_id, dnd, until))
                });
                match opened {
                    Ok((store, last_id, dnd, until)) => {
                        let _ = ready_tx.send(Ok((last_id, dnd, until)));
                        persistence_worker(store, receiver);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .context("start notification persistence worker")?;
        let (last_id, dnd, until) = ready_rx
            .await
            .context("notification persistence startup stopped")??;
        Ok((Self { commands }, last_id, dnd, until))
    }

    // Reserve before changing engine state. A full queue applies async
    // backpressure; cancellation while waiting has no side effects.
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
    pub(crate) async fn settings(
        &self,
        value: Option<super::settings::Settings>,
    ) -> Result<super::settings::Settings> {
        self.request(|response| PersistenceCommand::Settings { value, response })
            .await
    }
    pub(crate) async fn prune(&self) -> Result<usize> {
        self.request(|response| PersistenceCommand::Prune { response })
            .await
    }
    pub(crate) async fn save_once(
        &self,
        key: String,
        notification: ActiveNotification,
    ) -> Result<bool> {
        self.request(|response| PersistenceCommand::Once {
            key,
            notification: Box::new(notification),
            response,
        })
        .await
    }
    pub(crate) async fn targets(
        &self,
        app_key: Option<String>,
        selected: Option<Position>,
    ) -> Result<Vec<Position>> {
        self.request(|response| PersistenceCommand::Targets {
            app_key,
            selected,
            response,
        })
        .await
    }
    pub(crate) async fn delete(&self, positions: Vec<Position>) -> Result<()> {
        self.request(|response| PersistenceCommand::Delete {
            positions,
            response,
        })
        .await
    }
    pub(crate) async fn set_policy(
        &self,
        key: String,
        policy: super::policy::AppPolicy,
    ) -> Result<()> {
        self.request(|response| PersistenceCommand::Policy {
            key,
            policy,
            response,
        })
        .await
    }
    pub(crate) async fn policies(
        &self,
    ) -> Result<std::collections::BTreeMap<String, super::policy::AppPolicy>> {
        self.request(|response| PersistenceCommand::Policies { response })
            .await
    }
    pub(crate) async fn center(
        &self,
        query: CenterQuery,
        active: Vec<ActiveNotification>,
        epoch: String,
        revision: u64,
    ) -> Result<CenterPage> {
        self.request(|response| PersistenceCommand::Center {
            query: Box::new(query),
            active,
            epoch,
            revision,
            response,
        })
        .await
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
    pub(crate) async fn shutdown(&self) -> Result<()> {
        self.request(|response| PersistenceCommand::Shutdown { response })
            .await
    }
    #[cfg(test)]
    pub(super) async fn inject_fault(&self, fault: super::database::Fault) {
        self.request(|response| PersistenceCommand::Fault { fault, response })
            .await
            .unwrap();
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

// One slot covers an engine mutation, including eviction/expiry. Drop enqueues
// synchronously even if publication is cancelled after the in-memory change.
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
    let mut shutdowns = Vec::new();
    while let Some(command) = receiver.blocking_recv() {
        match command {
            // Accepted writes always run, even if their waiter was cancelled.
            PersistenceCommand::Settings { value, response } => {
                let write = value.is_some();
                reply(response, history_error.as_deref(), !write, || {
                    block_on(store.settings(value))
                });
            }
            PersistenceCommand::Prune { response } => {
                reply(response, history_error.as_deref(), false, || {
                    block_on(store.prune())
                })
            }
            PersistenceCommand::Once {
                key,
                notification,
                response,
            } => reply(response, history_error.as_deref(), false, || {
                block_on(store.save_once(&key, &notification))
            }),
            PersistenceCommand::Targets {
                app_key,
                selected,
                response,
            } => reply(response, history_error.as_deref(), true, || {
                block_on(store.targets(app_key.as_deref(), selected))
            }),
            PersistenceCommand::Delete {
                positions,
                response,
            } => reply(response, history_error.as_deref(), false, || {
                block_on(store.delete(&positions))
            }),
            PersistenceCommand::Policy {
                key,
                policy,
                response,
            } => reply(response, history_error.as_deref(), false, || {
                block_on(store.set_policy(&key, &policy))
            }),
            PersistenceCommand::Policies { response } => {
                reply(response, history_error.as_deref(), true, || {
                    block_on(store.policies())
                })
            }
            PersistenceCommand::Mutate(mutations) => {
                apply_mutations(&mut store, mutations, &mut history_error)
            }
            PersistenceCommand::Center {
                query,
                active,
                epoch,
                revision,
                response,
            } => {
                if !response.is_closed() {
                    let result = match history_error.as_deref() {
                        Some(error) => Err(anyhow!("{error}")),
                        None => recover_center_read(
                            &store.connection,
                            || response.is_closed(),
                            || block_on(store.center(&query, &active, &epoch, revision)),
                        ),
                    };
                    let _ = response.send(result);
                }
            }
            PersistenceCommand::Query {
                query,
                before,
                limit,
                excluded,
                response,
            } => reply(response, history_error.as_deref(), true, || {
                block_on(store.query(&query, before, limit, &excluded))
            }),
            // This unrevisioned legacy API is best-effort, but the store still
            // refuses every read after an uncertain transaction outcome.
            PersistenceCommand::List {
                before_history_id,
                limit,
                response,
            } => reply(response, None, true, || {
                block_on(store.list(before_history_id, limit))
            }),
            PersistenceCommand::Shutdown { response } => {
                receiver.close();
                shutdowns.push(response);
            }
            #[cfg(test)]
            PersistenceCommand::Fault { fault, response } => {
                store.fault = Some(fault);
                let _ = response.send(Ok(()));
            }
        }
    }
    let checkpoint = block_on(store.checkpoint());
    let error = history_error.or_else(|| checkpoint.err().map(|e| e.to_string()));
    // Release ownership before acknowledging, including when other handles live.
    drop(store);
    for response in shutdowns {
        let _ = response.send(match &error {
            Some(error) => Err(anyhow!("{error}")),
            None => Ok(()),
        });
    }
    if let Some(error) = error {
        tracing::warn!(%error, "notification persistence shutdown failed");
    }
}

fn apply_mutations(
    store: &mut NotificationStore,
    mutations: Vec<Mutation>,
    history_error: &mut Option<String>,
) {
    for mutation in mutations {
        if let Err(error) = block_on(store.apply(mutation)) {
            tracing::warn!(%error, "notification history update failed");
            // Once stale, never label storage with a newer engine revision.
            history_error.get_or_insert_with(|| error.to_string());
        }
    }
    if let Err(error) = block_on(store.prune()) {
        tracing::warn!(%error, "notification history retention failed");
        history_error.get_or_insert_with(|| error.to_string());
    }
}

fn reply<T>(
    response: oneshot::Sender<Result<T>>,
    history_error: Option<&str>,
    read_only: bool,
    operation: impl FnOnce() -> Result<T>,
) {
    if read_only && response.is_closed() {
        return;
    }
    let result = match history_error {
        Some(error) => Err(anyhow!("{error}")),
        None => operation(),
    };
    let _ = response.send(result);
}

// Retry only safe reads, never uncertain writes. All waiting is on the owner thread.
fn recover_center_read<T>(
    connection: &turso::Connection,
    cancelled: impl Fn() -> bool,
    mut read: impl FnMut() -> Result<T>,
) -> Result<T> {
    connection.busy_timeout(Duration::from_millis(50))?;
    let result = (|| {
        let mut delays = [Duration::from_millis(50), Duration::from_millis(100)].into_iter();
        loop {
            if cancelled() {
                return Err(anyhow!("Notification history read cancelled"));
            }
            match read() {
                Err(error) if transient_read_error(&error) => {
                    let Some(delay) = delays.next() else {
                        return Err(error);
                    };
                    std::thread::sleep(delay);
                }
                result => return result,
            }
        }
    })();
    let restored = connection.busy_timeout(Duration::from_secs(5));
    let value = result?;
    restored?;
    Ok(value)
}
fn transient_read_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<turso::Error>(),
            Some(turso::Error::Busy(_) | turso::Error::BusySnapshot(_))
        )
    })
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod tests;
