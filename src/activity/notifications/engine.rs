use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast};

use crate::{state::StateStore, time::unix_ms};

use super::{
    history::{self, HistoryError, HistoryPage, HistoryQuery},
    model::{
        ActiveNotification, HistoryNotification, IncomingNotification, NotificationActiveState,
        NotificationSignal, NotificationState, close_reason, expire_flag,
    },
    persistence::{NotificationPersistence, PendingWrites},
    policy::NotificationPolicy,
};

const CENTER_READ_TIMEOUT: Duration = Duration::from_secs(3);

struct EngineData {
    active: BTreeMap<u32, ActiveNotification>,
    dnd: bool,
    dnd_until_unix_ms: Option<u64>,
    history_revision: u64,
    app_policies: BTreeMap<String, super::policy::AppPolicy>,
    delete_tickets: BTreeMap<String, (u64, Vec<history::Position>)>,
    decks: super::decks::Decks,
    settings: super::settings::Settings,
    next_prune_ms: u64,
    retention_error: Option<String>,
    sharing: super::sharing::Sharing,
}

#[derive(Default)]
struct ExpiryBatch {
    expired_ids: Vec<u32>,
    changed: Vec<ActiveNotification>,
    dnd_expired: bool,
}

impl EngineData {
    fn upsert(
        &mut self,
        id: u32,
        incoming: IncomingNotification,
        source_monitor: String,
        now: u64,
        maximum_active: usize,
    ) -> (ActiveNotification, Option<u32>) {
        let mut evicted = None;
        let mut stored = if let Some(mut existing) = self.active.remove(&id) {
            let monitor = existing.source_monitor.clone();
            existing.replace_from(incoming, now);
            existing.source_monitor = monitor;
            existing
        } else {
            if self.active.len() >= maximum_active
                && let Some(oldest) = self
                    .active
                    .values()
                    .min_by_key(|item| (item.created_unix_ms, item.id))
                    .map(|item| item.id)
            {
                self.active.remove(&oldest);
                evicted = Some(oldest);
            }
            ActiveNotification::from_incoming(id, incoming, now)
        };
        if stored.source_monitor.is_empty() {
            stored.source_monitor = source_monitor;
        }
        let key = stored.app_key();
        if self
            .app_policies
            .get(&key)
            .is_some_and(|policy| policy.silenced(now))
        {
            stored.toast_visible = false;
        }
        if self.sharing.suppressed(now) {
            stored.toast_visible = false;
        }
        self.active.insert(id, stored.clone());
        self.history_revision = self.history_revision.wrapping_add(1);
        (stored, evicted)
    }

    fn expire(&mut self, now: u64) -> ExpiryBatch {
        let mut batch = ExpiryBatch::default();
        self.decks.expire(now, &mut self.active);
        self.sharing.expire(now);
        for policy in self.app_policies.values_mut() {
            if policy.silent && policy.expire(now) {
                self.history_revision = self.history_revision.wrapping_add(1);
            }
        }
        for notification in self.active.values_mut() {
            // Reading pauses popup lifetime only; sender protocol expiry remains authoritative.
            let held = self.decks.reading(&notification.source_monitor);
            let deadline = held
                .then(|| notification.toast_expires_unix_ms.take())
                .flatten();
            let (woke, expired) = notification.advance(now);
            if held {
                notification.toast_expires_unix_ms = deadline;
            }
            if expired {
                batch.expired_ids.push(notification.id);
            }
            if woke {
                batch.changed.push(notification.clone());
            }
        }
        batch.dnd_expired =
            self.dnd && expire_flag(&mut self.dnd, &mut self.dnd_until_unix_ms, now);
        // Popup-only changes do not invalidate persisted history.
        if !batch.changed.is_empty() {
            self.history_revision = self.history_revision.wrapping_add(1);
        }
        batch
    }
}

pub(crate) struct NotificationEngine {
    // Covers each mutation, persistence enqueue, signal, and state publication.
    // Keep this separate from data so internal reads never recursively lock it.
    mutations: Mutex<()>,
    data: Mutex<EngineData>,
    next_id: AtomicU32,
    ingress: Semaphore,
    expiry_wakeup: Notify,
    signals: broadcast::Sender<NotificationSignal>,
    state: StateStore,
    policy: NotificationPolicy,
    persistence: Option<NotificationPersistence>,
    history_epoch: String,
    history_readers: Semaphore,
    context_effects: Mutex<()>,
    // Internal targets cannot be supplied by freedesktop notification senders.
    update_targets: Mutex<BTreeMap<u32, String>>,
}

impl NotificationEngine {
    #[cfg(test)]
    pub(crate) async fn new(state: StateStore) -> Arc<Self> {
        Self::build(state, None, 0, false, None).await.unwrap()
    }

    #[cfg(test)]
    pub(crate) async fn catalog_fixture(
        records: Vec<ActiveNotification>,
        epoch: String,
        revision: u64,
    ) -> Arc<Self> {
        let mut engine = Self::new(StateStore::default()).await;
        let owned = Arc::get_mut(&mut engine).unwrap();
        owned.history_epoch = epoch;
        owned.next_id.store(
            records.iter().map(|n| n.id).max().unwrap_or(0),
            Ordering::Relaxed,
        );
        let data = owned.data.get_mut();
        data.history_revision = revision;
        data.active = records.into_iter().map(|n| (n.id, n)).collect();
        engine.publish_summary().await;
        engine
    }

    /// Keep the directory guard alive for the entire persistence/restart test.
    #[cfg(test)]
    pub(super) async fn persistent_fixture() -> (tempfile::TempDir, PathBuf, Arc<Self>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let engine = Self::persistent(StateStore::default(), path.clone())
            .await
            .unwrap();
        (dir, path, engine)
    }

    pub(crate) async fn persistent(state: StateStore, path: PathBuf) -> Result<Arc<Self>> {
        let (persistence, last_id, dnd, dnd_until_unix_ms) =
            tokio::task::spawn_blocking(move || NotificationPersistence::open(&path))
                .await
                .context("join notification database initialization")??;
        Self::build(state, Some(persistence), last_id, dnd, dnd_until_unix_ms).await
    }

    async fn build(
        state: StateStore,
        persistence: Option<NotificationPersistence>,
        last_id: u32,
        mut dnd: bool,
        mut dnd_until_unix_ms: Option<u64>,
    ) -> Result<Arc<Self>> {
        super::identity::refresh()
            .await
            .context("load notification application icons")?;
        let dnd_expired = expire_flag(&mut dnd, &mut dnd_until_unix_ms, unix_ms());
        let (signals, _) = broadcast::channel(256);
        let mut app_policies = if let Some(store) = &persistence {
            store.policies().await?
        } else {
            BTreeMap::new()
        };
        for policy in app_policies.values_mut() {
            policy.expire(unix_ms());
        }
        let settings = if let Some(store) = &persistence {
            store.settings(None).await?
        } else {
            Default::default()
        };
        let engine = Arc::new(Self {
            mutations: Mutex::new(()),
            data: Mutex::new(EngineData {
                active: BTreeMap::new(),
                dnd,
                dnd_until_unix_ms,
                history_revision: 0,
                app_policies,
                delete_tickets: BTreeMap::new(),
                decks: super::decks::Decks::default(),
                settings,
                next_prune_ms: unix_ms().saturating_add(60_000),
                retention_error: None,
                sharing: Default::default(),
            }),
            next_id: AtomicU32::new(last_id),
            ingress: Semaphore::new(256),
            expiry_wakeup: Notify::new(),
            signals,
            state,
            policy: NotificationPolicy::default(),
            persistence,
            history_epoch: history::new_epoch()?,
            history_readers: Semaphore::new(2),
            context_effects: Mutex::new(()),
            update_targets: Mutex::new(BTreeMap::new()),
        });
        if dnd_expired && let Some(persistence) = &engine.persistence {
            persistence.reserve().await?.set_dnd(false, None);
        }
        engine.publish_summary().await;
        Ok(engine)
    }

    async fn reserve_persistence(&self) -> Result<Option<PendingWrites<'_>>> {
        match &self.persistence {
            Some(persistence) => Ok(Some(persistence.reserve().await?)),
            None => Ok(None),
        }
    }

    pub(crate) fn subscribe_signals(&self) -> broadcast::Receiver<NotificationSignal> {
        self.signals.subscribe()
    }

    pub(crate) async fn notify(
        &self,
        replaces_id: u32,
        notification: IncomingNotification,
    ) -> Result<u32> {
        let _permit = self
            .ingress
            .try_acquire()
            .context("notification ingress is full")?;
        self.policy.validate(&notification)?;
        if let Err(error) = super::identity::refresh().await {
            tracing::warn!(%error, "desktop icon refresh failed; using previous registry");
        }
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let now = unix_ms();
        let source_monitor = self
            .state
            .read(|state| state.workspaces.focused_monitor.clone().unwrap_or_default())
            .await;
        let (id, stored, evicted) = {
            let mut data = self.data.lock().await;
            let id = if replaces_id != 0 && data.active.contains_key(&replaces_id) {
                replaces_id
            } else {
                self.allocate_id(&data.active)?
            };
            let (stored, evicted) = data.upsert(
                id,
                notification,
                source_monitor,
                now,
                self.policy.maximum_active,
            );
            (id, stored, evicted)
        };
        // A third-party replacement must never inherit an internal update route.
        self.update_targets.lock().await.remove(&id);
        if let Some(persistence) = &mut writes {
            persistence.save(stored);
        }
        if let Some(id) = evicted {
            self.emit(NotificationSignal::Closed {
                id,
                reason: close_reason::UNDEFINED,
            });
            if let Some(persistence) = &mut writes {
                persistence.close(id, now, close_reason::UNDEFINED);
            }
        }
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        Ok(id)
    }

    /// Durable, idempotent update outcomes; publish only after storage ack.
    pub(crate) async fn notify_update_once(
        &self,
        key: String,
        run_id: String,
        incoming: IncomingNotification,
    ) -> Result<()> {
        anyhow::ensure!(
            key.len() <= 1024
                && (run_id.is_empty() || shelllist_update_status::valid_run_id(&run_id)),
            "Invalid update notification identity"
        );
        self.policy.validate(&incoming)?;
        let _mutation = self.mutations.lock().await;
        let store = self
            .persistence
            .as_ref()
            .context("Update notifications require persistent storage")?;
        let mut data = self.data.lock().await;
        let id = self.allocate_id(&data.active)?;
        let now = unix_ms();
        let mut stored = ActiveNotification::from_incoming(id, incoming, now);
        stored.source_monitor = self
            .state
            .read(|s| s.workspaces.focused_monitor.clone().unwrap_or_default())
            .await;
        if data
            .app_policies
            .get(&stored.app_key())
            .is_some_and(|p| p.silenced(now))
        {
            stored.toast_visible = false;
        }
        if data.sharing.suppressed(now) {
            stored.toast_visible = false;
        }
        if !store.save_once(key, stored.clone()).await? {
            return Ok(());
        }
        if data.active.len() >= self.policy.maximum_active {
            // Use the normal close path, including persistence, for eviction.
            let oldest = data
                .active
                .values()
                .min_by_key(|n| (n.created_unix_ms, n.id))
                .map(|n| n.id);
            drop(data);
            if let Some(oldest) = oldest {
                let mut writes = self.reserve_persistence().await?;
                self.close_locked(oldest, close_reason::UNDEFINED, &mut writes)
                    .await;
            }
            data = self.data.lock().await;
        }
        data.active.insert(id, stored);
        data.history_revision = data.history_revision.wrapping_add(1);
        let mut targets = self.update_targets.lock().await;
        targets.retain(|id, _| data.active.contains_key(id));
        targets.insert(id, run_id);
        drop(targets);
        drop(data);
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        Ok(())
    }

    pub(crate) async fn observe_sharing(&self, observation: Option<bool>) {
        let _mutation = self.mutations.lock().await;
        let mut data = self.data.lock().await;
        data.sharing.observe(observation);
        if data.sharing.suppressed(unix_ms()) {
            for n in data.active.values_mut() {
                n.toast_visible = false;
                n.toast_expires_unix_ms = None;
            }
        }
        drop(data);
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
    }

    pub(crate) async fn snooze_sharing(
        &self,
        generation: u64,
        minutes: u16,
    ) -> Result<NotificationState> {
        let _mutation = self.mutations.lock().await;
        self.data
            .lock()
            .await
            .sharing
            .snooze(generation, minutes, unix_ms())?;
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        Ok(self.state.read(|s| s.notifications.clone()).await)
    }

    pub(crate) async fn set_settings(
        self: &Arc<Self>,
        patch: super::settings::SettingsPatch,
    ) -> Result<NotificationState> {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let _mutation = engine.mutations.lock().await;
            let settings = engine.data.lock().await.settings.patched(patch)?;
            if let Some(store) = &engine.persistence {
                store.settings(Some(settings.clone())).await?;
            }
            let mut data = engine.data.lock().await;
            data.settings = settings;
            data.history_revision = data.history_revision.wrapping_add(1);
            drop(data);
            engine.publish_summary().await;
            Ok(engine.state.read(|s| s.notifications.clone()).await)
        })
        .await
        .context("join notification settings operation")?
    }

    pub(crate) async fn set_app_policy(
        self: &Arc<Self>,
        key: String,
        policy: super::policy::AppPolicy,
    ) -> Result<NotificationState> {
        anyhow::ensure!(
            !key.is_empty()
                && key.len() <= 1100
                && ["desktop:", "named:", "unknown:"]
                    .iter()
                    .any(|prefix| key.starts_with(prefix)),
            "invalid notification app key"
        );
        policy.validate()?;
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let _mutation = engine.mutations.lock().await;
            {
                let data = engine.data.lock().await;
                anyhow::ensure!(
                    data.app_policies.contains_key(&key) || data.app_policies.len() < 256,
                    "Application policy limit reached"
                );
            }
            if let Some(store) = &engine.persistence {
                store.set_policy(key.clone(), policy.clone()).await?;
            }
            let mut data = engine.data.lock().await;
            if policy.silenced(unix_ms()) {
                for notification in data.active.values_mut() {
                    if notification.app_key() == key {
                        // Clearing silence must not replay an old popup.
                        notification.toast_visible = false;
                        notification.toast_expires_unix_ms = None;
                    }
                }
            }
            data.app_policies.insert(key, policy);
            data.history_revision = data.history_revision.wrapping_add(1);
            drop(data);
            engine.expiry_wakeup.notify_one();
            engine.publish_summary().await;
            Ok(engine.state.read(|state| state.notifications.clone()).await)
        })
        .await
        .context("join app policy operation")?
    }

    pub(crate) async fn prepare_delete(
        &self,
        app_key: Option<String>,
        selected: Option<history::Position>,
    ) -> Result<super::model::DeleteConfirmation> {
        anyhow::ensure!(
            app_key
                .as_ref()
                .is_none_or(|key| !key.is_empty() && key.len() <= 4096)
                && selected.is_none_or(|p| p.id > 0 && p.created > 0),
            "Invalid delete scope"
        );
        let _mutation = self.mutations.lock().await;
        let mut records = if let Some(store) = &self.persistence {
            store.targets(app_key.clone(), selected).await?
        } else {
            Vec::new()
        };
        let data = self.data.lock().await;
        for n in data.active.values() {
            if app_key.as_ref().is_none_or(|key| key == &n.app_key())
                && selected.is_none_or(|p| p.id == n.id && p.created == n.created_unix_ms)
            {
                records.push(history::Position {
                    id: n.id,
                    created: n.created_unix_ms,
                });
            }
        }
        drop(data);
        records.sort_by_key(|p| (p.created, p.id));
        records.dedup_by_key(|p| (p.created, p.id));
        anyhow::ensure!(!records.is_empty(), "No notifications to delete");
        let token = history::new_epoch()?;
        let expires = unix_ms() + 60_000;
        let count = records.len();
        let mut data = self.data.lock().await;
        data.delete_tickets
            .retain(|_, (until, _)| *until > unix_ms());
        anyhow::ensure!(
            data.delete_tickets.len() < 8,
            "Too many pending delete confirmations"
        );
        data.delete_tickets
            .insert(token.clone(), (expires, records));
        Ok(super::model::DeleteConfirmation {
            token,
            count,
            app_key,
            selected,
            expires_unix_ms: expires,
        })
    }

    pub(crate) async fn cancel_delete(&self, token: &str) {
        self.data.lock().await.delete_tickets.remove(token);
    }

    pub(crate) async fn delete_confirmed(self: &Arc<Self>, token: String) -> Result<usize> {
        let engine = Arc::clone(self);
        // An admitted deletion finishes even if the IPC caller disconnects.
        tokio::spawn(async move {
            let _mutation = engine.mutations.lock().await;
            let (expires, positions) = engine
                .data
                .lock()
                .await
                .delete_tickets
                .remove(&token)
                .context("Delete confirmation expired; request a new confirmation")?;
            anyhow::ensure!(
                expires > unix_ms(),
                "Delete confirmation expired; request a new confirmation"
            );
            // Commit persistent removal before publishing/removing active records.
            // Queue ordering includes every prior accepted notification write.
            if let Some(store) = &engine.persistence {
                store.delete(positions.clone()).await?;
            }
            let mut data = engine.data.lock().await;
            for position in &positions {
                if data
                    .active
                    .get(&position.id)
                    .is_some_and(|n| n.created_unix_ms == position.created)
                {
                    data.active.remove(&position.id);
                    engine.emit(NotificationSignal::Closed {
                        id: position.id,
                        reason: close_reason::DISMISSED,
                    });
                }
            }
            data.history_revision = data.history_revision.wrapping_add(1);
            drop(data);
            engine.expiry_wakeup.notify_one();
            engine.publish_summary().await;
            Ok(positions.len())
        })
        .await
        .context("join notification deletion")?
    }

    pub(crate) async fn close(&self, id: u32, reason: u32) -> Result<bool> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let removed = self.close_locked(id, reason, &mut writes).await;
        if removed {
            self.publish_summary().await;
        }
        Ok(removed)
    }

    // Caller holds mutations and a reserved batch, including expiry/actions.
    async fn close_locked(
        &self,
        id: u32,
        reason: u32,
        writes: &mut Option<PendingWrites<'_>>,
    ) -> bool {
        let removed = {
            let mut data = self.data.lock().await;
            let removed = data.active.remove(&id).is_some();
            if removed {
                data.history_revision = data.history_revision.wrapping_add(1);
            }
            removed
        };
        if removed {
            self.update_targets.lock().await.remove(&id);
            if let Some(persistence) = writes {
                persistence.close(id, unix_ms(), reason);
            }
            self.emit(NotificationSignal::Closed { id, reason });
            self.expiry_wakeup.notify_one();
        }
        removed
    }

    pub(crate) async fn dismiss(&self, id: u32) -> Result<bool> {
        self.close(id, close_reason::DISMISSED).await
    }

    pub(crate) async fn clear(&self) -> Result<usize> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let ids = {
            let mut data = self.data.lock().await;
            let ids = data.active.keys().copied().collect::<Vec<_>>();
            data.active.clear();
            if !ids.is_empty() {
                data.history_revision = data.history_revision.wrapping_add(1);
            }
            ids
        };
        self.update_targets.lock().await.clear();
        if let Some(persistence) = &mut writes
            && !ids.is_empty()
        {
            persistence.clear(unix_ms(), close_reason::DISMISSED);
        }
        for id in &ids {
            self.emit(NotificationSignal::Closed {
                id: *id,
                reason: close_reason::DISMISSED,
            });
        }
        if !ids.is_empty() {
            self.expiry_wakeup.notify_one();
            self.publish_summary().await;
        }
        Ok(ids.len())
    }

    pub(crate) async fn set_dnd(
        &self,
        enabled: bool,
        until_unix_ms: Option<u64>,
    ) -> Result<NotificationState> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        self.set_dnd_locked(enabled, until_unix_ms, &mut writes)
            .await;
        Ok(self.state.read(|state| state.notifications.clone()).await)
    }

    async fn set_dnd_locked(
        &self,
        enabled: bool,
        until_unix_ms: Option<u64>,
        writes: &mut Option<PendingWrites<'_>>,
    ) {
        let until = enabled.then_some(until_unix_ms).flatten();
        let changed = {
            let mut data = self.data.lock().await;
            let changed = data.dnd != enabled || data.dnd_until_unix_ms != until;
            data.dnd = enabled;
            data.dnd_until_unix_ms = until;
            changed
        };
        if changed {
            if let Some(persistence) = writes {
                persistence.set_dnd(enabled, until);
            }
            self.expiry_wakeup.notify_one();
            self.publish_summary().await;
        }
    }

    pub(crate) async fn toggle_dnd(&self) -> Result<bool> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let enabled = !self.data.lock().await.dnd;
        self.set_dnd_locked(enabled, None, &mut writes).await;
        Ok(enabled)
    }

    pub(crate) async fn snooze(&self, id: u32, until_unix_ms: u64) -> Result<bool> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let now = unix_ms();
        if until_unix_ms <= now {
            return Ok(false);
        }
        let stored = {
            let mut data = self.data.lock().await;
            let Some(notification) = data.active.get_mut(&id) else {
                return Ok(false);
            };
            notification.snoozed_until_unix_ms = Some(until_unix_ms);
            notification.updated_unix_ms = now;
            notification.toast_visible = true;
            notification.toast_expires_unix_ms = Some(until_unix_ms.saturating_add(5_000));
            if notification.expires_unix_ms.is_some() {
                notification.expires_unix_ms = Some(until_unix_ms.saturating_add(5_000));
            }
            let stored = notification.clone();
            data.history_revision = data.history_revision.wrapping_add(1);
            stored
        };
        if let Some(persistence) = &mut writes {
            persistence.save(stored);
        }
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        Ok(true)
    }

    pub(crate) async fn clear_group(&self, group_key: &str) -> Result<usize> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let ids = {
            let data = self.data.lock().await;
            data.active
                .values()
                .filter(|item| item.group_key == group_key)
                .map(|item| item.id)
                .collect::<Vec<_>>()
        };
        for id in &ids {
            self.close_locked(*id, close_reason::DISMISSED, &mut writes)
                .await;
        }
        if !ids.is_empty() {
            self.publish_summary().await;
        }
        Ok(ids.len())
    }

    #[cfg(test)]
    pub(crate) async fn active(&self) -> Vec<ActiveNotification> {
        self.data.lock().await.active.values().cloned().collect()
    }

    pub(crate) async fn history(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        // A subscriber can query immediately after publication, before the
        // mutation's reserved write batch is dropped/enqueued. Wait for that
        // boundary so the published revision never leads its history data.
        let _mutation = self.mutations.lock().await;
        let Some(persistence) = &self.persistence else {
            return Ok(Vec::new());
        };
        persistence.list(before_history_id, limit).await
    }

    // Call while holding mutations: the revision and overlay must describe the
    // same persistence boundary as the subsequent storage read.
    async fn catalog_snapshot(&self) -> (u64, Vec<ActiveNotification>) {
        let data = self.data.lock().await;
        (
            data.history_revision,
            data.active
                .values()
                .filter(|n| n.snoozed_until_unix_ms.is_none())
                .cloned()
                .collect(),
        )
    }

    pub(crate) async fn query_center(
        &self,
        mut query: super::center::CenterQuery,
    ) -> Result<super::center::CenterPage, HistoryError> {
        query.normalize()?;
        // Brief contention is native-owned recovery, not a frontend retry loop.
        // One deadline covers admission, the mutation boundary and storage read.
        let deadline = tokio::time::Instant::now() + CENTER_READ_TIMEOUT;
        let _reader = tokio::time::timeout_at(deadline, self.history_readers.acquire())
            .await
            .map_err(|_| HistoryError::Busy)?
            .map_err(|_| HistoryError::Busy)?;
        tokio::time::timeout_at(deadline, self.query_center_admitted(query))
            .await
            .map_err(|_| {
                HistoryError::Unavailable(anyhow::anyhow!(
                    "Notification history read timed out; refresh to try again"
                ))
            })?
    }

    async fn query_center_admitted(
        &self,
        query: super::center::CenterQuery,
    ) -> Result<super::center::CenterPage, HistoryError> {
        let _mutation = self.mutations.lock().await;
        let (revision, active) = self.catalog_snapshot().await;
        query.validate_revision(&self.history_epoch, revision)?;
        if let Some(persistence) = &self.persistence {
            return persistence
                .center(query, active, self.history_epoch.clone(), revision)
                .await
                .map_err(|error| {
                    error
                        .downcast::<HistoryError>()
                        .unwrap_or_else(HistoryError::Unavailable)
                });
        }
        super::center::project(
            active
                .iter()
                .map(|n| super::center::Preview::from_active(n, &query.query))
                .collect(),
            &query,
            &self.history_epoch,
            revision,
            |preview| {
                active
                    .iter()
                    .find(|n| n.id == preview.id && n.created_unix_ms == preview.created_unix_ms)
                    .cloned()
                    .map(Into::into)
                    .ok_or_else(|| anyhow::anyhow!("Notification unavailable"))
            },
        )
    }

    pub(crate) async fn query_history(
        &self,
        mut query: HistoryQuery,
    ) -> Result<HistoryPage, HistoryError> {
        query.normalize()?;
        let _reader = self
            .history_readers
            .try_acquire()
            .map_err(|_| HistoryError::Busy)?;
        // The revision, active overlay and persisted rows are one read. This
        // also waits for the preceding mutation's reserved writes to enqueue.
        let _mutation = self.mutations.lock().await;
        let (revision, active) = self.catalog_snapshot().await;
        let now = unix_ms();
        let before = query.position(&self.history_epoch, revision, now)?;
        let persisted = if let Some(persistence) = &self.persistence {
            persistence
                .query(
                    query.query.clone(),
                    before,
                    query.limit,
                    active
                        .iter()
                        .map(|item| (item.created_unix_ms, item.id))
                        .collect(),
                )
                .await
                .map_err(HistoryError::Unavailable)?
        } else {
            Vec::new()
        };
        history::page(
            persisted,
            active,
            &query,
            before,
            &self.history_epoch,
            revision,
            now,
        )
    }

    pub(crate) async fn reply(&self, id: u32, text: &str) -> bool {
        // Serialize capability validation and signal emission with close/action.
        let _mutation = self.mutations.lock().await;
        if text.trim().is_empty() || text.len() > 4096 {
            return false;
        }
        if !self
            .data
            .lock()
            .await
            .active
            .get(&id)
            .is_some_and(|notification| {
                notification
                    .actions
                    .iter()
                    .any(|action| action.key == "inline-reply")
            })
        {
            return false;
        }
        self.emit(NotificationSignal::Replied {
            id,
            text: text.into(),
        });
        true
    }

    pub(crate) async fn invoke_action(
        &self,
        id: u32,
        action_key: &str,
        token: Option<String>,
    ) -> Result<bool> {
        let _mutation = self.mutations.lock().await;
        let mut writes = self.reserve_persistence().await?;
        let resident = {
            let data = self.data.lock().await;
            let Some(notification) = data.active.get(&id) else {
                return Ok(false);
            };
            if !notification
                .actions
                .iter()
                .any(|action| action.key == action_key)
            {
                return Ok(false);
            }
            notification.hints.resident
        };
        if action_key == "default"
            && let Some(run_id) = self.update_targets.lock().await.get(&id).cloned()
        {
            crate::updates::open(&run_id, token.as_deref()).await?;
        }
        if let Some(token) = token {
            self.emit(NotificationSignal::ActivationToken { id, token });
        }
        self.emit(NotificationSignal::ActionInvoked {
            id,
            action_key: action_key.into(),
        });
        if !resident {
            self.close_locked(id, close_reason::DISMISSED, &mut writes)
                .await;
            self.publish_summary().await;
        }
        Ok(true)
    }

    pub(crate) async fn context_action(
        &self,
        id: u32,
        created: u64,
        updated: u64,
        key: &str,
    ) -> Result<bool> {
        let _effect = self
            .context_effects
            .try_lock()
            .context("Notification action already pending")?;
        let (record, seconds) = {
            let _mutation = self.mutations.lock().await;
            let data = self.data.lock().await;
            let record = data
                .active
                .get(&id)
                .filter(|n| {
                    n.created_unix_ms == created
                        && n.updated_unix_ms == updated
                        && n.expires_unix_ms.is_none_or(|until| until > unix_ms())
                })
                .context("Notification changed or expired")?
                .clone();
            (record, data.settings.code_expiry_seconds)
        };
        if key == "focus" {
            super::context_actions::focus(&record.hints.desktop_entry).await?;
            return Ok(false);
        }
        let target = super::context_actions::extract(&record)
            .into_iter()
            .find(|t| t.descriptor().key == key)
            .context("Notification action unavailable")?;
        super::context_actions::execute(target, seconds).await
    }

    pub(crate) async fn set_reading(&self, monitor: String, reading: bool) -> Result<()> {
        anyhow::ensure!(
            !monitor.is_empty() && monitor.len() <= 256,
            "Invalid toast monitor"
        );
        anyhow::ensure!(
            self.state
                .read(|s| s.workspaces.monitors.iter().any(|m| m.name == monitor))
                .await,
            "Toast monitor unavailable"
        );
        let _mutation = self.mutations.lock().await;
        let mut data = self.data.lock().await;
        let EngineData { decks, active, .. } = &mut *data;
        decks.set_reading(monitor, reading, unix_ms(), active);
        drop(data);
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        Ok(())
    }

    pub(crate) async fn run_expiry(self: Arc<Self>) {
        loop {
            let delay = self.next_expiry_delay().await;
            match delay {
                Some(delay) => {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => self.expire_due().await,
                        _ = self.expiry_wakeup.notified() => {}
                    }
                }
                None => self.expiry_wakeup.notified().await,
            }
        }
    }

    async fn next_expiry_delay(&self) -> Option<Duration> {
        let data = self.data.lock().await;
        let notification_wakeup = data.active.values().filter_map(|n| {
            if data.decks.reading(&n.source_monitor) {
                n.snoozed_until_unix_ms.or(n.expires_unix_ms)
            } else {
                n.next_deadline()
            }
        });
        let next = notification_wakeup
            .chain(data.decks.deadline())
            .chain(data.sharing.until_unix_ms)
            .chain(Some(data.next_prune_ms))
            .chain(data.dnd_until_unix_ms)
            .chain(
                data.app_policies
                    .values()
                    .filter(|p| p.silent)
                    .filter_map(|p| p.until_unix_ms),
            )
            .min()?;
        Some(Duration::from_millis(next.saturating_sub(unix_ms())))
    }

    async fn expire_due(&self) {
        let _mutation = self.mutations.lock().await;
        let mut writes = match self.reserve_persistence().await {
            Ok(writes) => writes,
            Err(error) => {
                tracing::warn!(%error, "notification expiry persistence unavailable");
                // Don't spin on an already-due expiry after worker failure.
                tokio::time::sleep(Duration::from_secs(1)).await;
                return;
            }
        };
        let prune_due = self.data.lock().await.next_prune_ms <= unix_ms();
        if prune_due {
            let result = match &self.persistence {
                Some(store) => store.prune().await,
                None => Ok(0),
            };
            let mut data = self.data.lock().await;
            data.next_prune_ms = unix_ms().saturating_add(60_000);
            match result {
                Ok(removed) => {
                    data.retention_error = None;
                    if removed > 0 {
                        data.history_revision = data.history_revision.wrapping_add(1);
                    }
                }
                Err(error) => {
                    data.retention_error = Some(format!("Notification retention failed: {error}"))
                }
            }
        }
        let batch = self.data.lock().await.expire(unix_ms());
        if let Some(persistence) = &mut writes {
            for notification in batch.changed {
                persistence.save(notification);
            }
            if batch.dnd_expired {
                persistence.set_dnd(false, None);
            }
        }
        for id in batch.expired_ids {
            self.close_locked(id, close_reason::EXPIRED, &mut writes)
                .await;
        }
        self.publish_summary().await;
    }

    fn allocate_id(&self, active: &BTreeMap<u32, ActiveNotification>) -> Result<u32> {
        for _ in 0..u32::MAX {
            let id = self
                .next_id
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1)
                .max(1);
            if !active.contains_key(&id) {
                return Ok(id);
            }
        }
        bail!("notification ID space is exhausted")
    }

    fn emit(&self, signal: NotificationSignal) {
        let _ = self.signals.send(signal);
    }

    async fn publish_summary(&self) {
        let workspaces = self.state.read(|s| s.workspaces.clone()).await;
        let (summary, active) = {
            let mut data = self.data.lock().await;
            for n in data.active.values_mut() {
                if !workspaces
                    .monitors
                    .iter()
                    .any(|m| m.name == n.source_monitor)
                {
                    n.source_monitor = workspaces
                        .focused_monitor
                        .clone()
                        .filter(|name| workspaces.monitors.iter().any(|m| &m.name == name))
                        .or_else(|| workspaces.monitors.first().map(|m| m.name.clone()))
                        .unwrap_or_default();
                }
            }
            let visible = data
                .active
                .values()
                .filter(|item| item.snoozed_until_unix_ms.is_none())
                .map(|item| {
                    let mut item = item.clone();
                    let key = item.app_key();
                    if let Some(policy) = data.app_policies.get(&key) {
                        item.dnd_bypass = policy.bypass_dnd;
                        if policy.silenced(unix_ms()) {
                            item.toast_visible = false;
                            item.hints.suppress_sound = true;
                        }
                    }
                    if data.sharing.suppressed(unix_ms()) {
                        item.toast_visible = false;
                    }
                    item
                })
                .collect::<Vec<_>>();
            let dnd = data.dnd;
            let toast_decks = data.decks.project(&visible, dnd);
            let count = visible.len().try_into().unwrap_or(u32::MAX);
            let noun = if count == 1 {
                "Notification"
            } else {
                "Notifications"
            };
            let class_name = if data.dnd {
                "dnd-notification"
            } else {
                "notification"
            };
            (
                NotificationState {
                    available: true,
                    count,
                    dnd: data.dnd,
                    dnd_until_unix_ms: data.dnd_until_unix_ms,
                    inhibited: false,
                    text: count.to_string(),
                    tooltip: format!("{count} {noun}"),
                    alt: class_name.into(),
                    class_name: class_name.into(),
                    backend: "native".into(),
                    history_revision: data.history_revision,
                    settings: Some(data.settings.clone()),
                    sharing: Some(data.sharing.clone()),
                    app_policies: data.app_policies.clone(),
                    error: data.retention_error.clone(),
                },
                NotificationActiveState {
                    available: true,
                    revision: data.history_revision,
                    notifications: visible,
                    toast_decks,
                    error: None,
                },
            )
        };
        self.state.update_notifications(summary).await;
        self.state.update_notification_active(active).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::state::StateStore;

    use super::NotificationEngine;
    use crate::activity::notifications::model::{IncomingNotification, close_reason};

    fn notification(summary: &str, timeout_ms: i32) -> IncomingNotification {
        let mut n = super::super::model::incoming("test", summary, "");
        n.expire_timeout = timeout_ms;
        n
    }

    #[tokio::test]
    async fn update_outcomes_commit_once_across_dismissal_and_restart() {
        let (_directory, path, engine) = NotificationEngine::persistent_fixture().await;
        engine.toggle_dnd().await.unwrap();
        for _ in 0..3 {
            engine
                .notify_update_once(
                    "updates:run:run-1".into(),
                    "run-1".into(),
                    notification("Staged, not activated", -1),
                )
                .await
                .unwrap();
        }
        assert_eq!(engine.active().await.len(), 1);
        assert!(engine.state.read(|s| s.notifications.dnd).await);
        let id = engine.active().await[0].id;
        assert!(engine.update_targets.lock().await.contains_key(&id));
        engine
            .notify(id, notification("External replacement", -1))
            .await
            .unwrap();
        assert!(
            !engine.update_targets.lock().await.contains_key(&id),
            "external replacement cannot inherit an internal route"
        );
        engine.clear().await.unwrap();
        assert_eq!(engine.history(None, 100).await.unwrap().len(), 1);
        drop(engine);
        let engine = NotificationEngine::persistent(StateStore::default(), path)
            .await
            .unwrap();
        engine
            .notify_update_once(
                "updates:run:run-1".into(),
                "run-1".into(),
                notification("Must not replay", -1),
            )
            .await
            .unwrap();
        assert!(engine.active().await.is_empty());
        assert_eq!(engine.history(None, 100).await.unwrap().len(), 1);
        engine
            .notify_update_once(
                "updates:run:run-2".into(),
                "run-2".into(),
                notification("New outcome", -1),
            )
            .await
            .unwrap();
        assert_eq!(engine.active().await.len(), 1);
        assert_eq!(engine.history(None, 100).await.unwrap().len(), 2);
    }

    #[test]
    fn expiry_keeps_snooze_popup_and_protocol_deadlines_distinct() {
        use super::super::policy::AppPolicy;
        use super::{ActiveNotification, EngineData};
        let mut data = EngineData {
            active: Default::default(),
            dnd: true,
            dnd_until_unix_ms: Some(100),
            history_revision: u64::MAX,
            app_policies: Default::default(),
            delete_tickets: Default::default(),
            decks: Default::default(),
            settings: Default::default(),
            next_prune_ms: u64::MAX,
            retention_error: None,
            sharing: Default::default(),
        };
        for (id, snooze, expiry) in [
            (1, Some(101), Some(99)),
            (2, Some(100), Some(100)),
            (3, None, None),
        ] {
            let mut item = ActiveNotification::from_incoming(id, notification("deadline", 0), 0);
            item.snoozed_until_unix_ms = snooze;
            item.toast_expires_unix_ms = Some(100);
            item.expires_unix_ms = expiry;
            data.active.insert(id, item);
        }
        for silent in [false, true] {
            data.app_policies.insert(
                silent.to_string(),
                AppPolicy {
                    silent,
                    until_unix_ms: Some(100),
                    ..Default::default()
                },
            );
        }
        let batch = data.expire(100);
        assert_eq!(batch.expired_ids, [2]);
        assert_eq!(
            batch.changed.as_slice(),
            std::slice::from_ref(&data.active[&2])
        );
        assert_eq!(data.active[&1].next_deadline(), Some(101));
        assert!(data.active[&1].toast_visible);
        assert_eq!(data.active[&1].toast_expires_unix_ms, Some(100));
        assert!(data.active[&2].snoozed_until_unix_ms.is_none());
        assert!(!data.active[&2].toast_visible);
        assert!(!data.active[&3].toast_visible);
        assert!(data.active[&3].next_deadline().is_none());
        assert!(batch.dnd_expired);
        assert!(!data.dnd);
        assert!(data.dnd_until_unix_ms.is_none());
        assert_eq!(data.history_revision, 1); // one policy + one wake; popup-only changes excluded
        let again = data.expire(100);
        assert!(again.changed.is_empty() && !again.dnd_expired);
        assert_eq!(data.history_revision, 1);
        // Startup also cleans expired deadlines on disabled policies; runtime does not.
        let disabled = data.app_policies.get_mut("false").unwrap();
        assert_eq!(disabled.until_unix_ms, Some(100));
        assert!(disabled.expire(100));
        assert!(!disabled.silent && disabled.until_unix_ms.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_mutations_keep_persistence_and_snapshot_consistent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let state = StateStore::default();
        let engine = NotificationEngine::persistent(state.clone(), path.clone())
            .await
            .unwrap();
        for round in 0..1_000 {
            let mut tasks = tokio::task::JoinSet::new();
            for worker in 0..4 {
                let engine = Arc::clone(&engine);
                tasks.spawn(async move {
                    if worker == 0 {
                        engine.clear().await.unwrap();
                    } else {
                        engine
                            .notify(1, notification(&format!("{round}-{worker}"), 0))
                            .await
                            .unwrap();
                    }
                });
            }
            while let Some(task) = tasks.join_next().await {
                task.unwrap();
            }
            // Listing is also a barrier for the persistence worker.
            let mut stored = engine
                .history(None, 100)
                .await
                .unwrap()
                .into_iter()
                .filter(|item| item.closed_unix_ms.is_none())
                .map(|item| item.notification)
                .collect::<Vec<_>>();
            stored.sort_by_key(|item| item.id);
            let active = engine.active().await;
            assert_eq!(stored, active, "persistence diverged at round {round}");
            let snapshot = state.snapshot().await;
            assert_eq!(snapshot.notifications.count as usize, active.len());
            assert_eq!(snapshot.notification_active.notifications, active);
        }
        let last_id = engine
            .active()
            .await
            .iter()
            .map(|item| item.id)
            .max()
            .unwrap_or(0);
        drop(engine);
        let restarted = NotificationEngine::persistent(StateStore::default(), path)
            .await
            .unwrap();
        assert!(
            restarted.active().await.is_empty(),
            "a new server session must not revive old actions"
        );
        assert!(
            restarted
                .history(None, 100)
                .await
                .unwrap()
                .iter()
                .all(|item| item.closed_unix_ms.is_some())
        );
        assert!(
            restarted
                .notify(0, notification("new session", 0))
                .await
                .unwrap()
                > last_id
        );
    }

    #[tokio::test(start_paused = true)]
    async fn center_waits_for_capacity_but_bounds_reads_and_releases_cancelled_owners() {
        use super::HistoryError;
        let engine = NotificationEngine::new(StateStore::default()).await;
        let query = || serde_json::from_value(serde_json::json!({"view":"apps"})).unwrap();
        let permits = engine.history_readers.acquire_many(2).await.unwrap();
        let reader = Arc::clone(&engine);
        let task = tokio::spawn(async move { reader.query_center(query()).await });
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "brief contention should wait natively");
        drop(permits);
        assert!(task.await.unwrap().is_ok());
        assert_eq!(engine.history_readers.available_permits(), 2);

        let permits = engine.history_readers.acquire_many(2).await.unwrap();
        assert!(matches!(
            engine.query_center(query()).await,
            Err(HistoryError::Busy)
        ));
        drop(permits);
        let mutation = engine.mutations.lock().await;
        assert!(matches!(
            engine.query_center(query()).await,
            Err(HistoryError::Unavailable(_))
        ));
        assert_eq!(
            engine.history_readers.available_permits(),
            2,
            "deadline releases read admission"
        );
        let reader = Arc::clone(&engine);
        let task = tokio::spawn(async move { reader.query_center(query()).await });
        tokio::task::yield_now().await;
        assert_eq!(engine.history_readers.available_permits(), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(mutation);
        assert_eq!(engine.history_readers.available_permits(), 2);
        assert!(engine.query_center(query()).await.is_ok());
    }

    #[tokio::test]
    async fn stopped_persistence_rejects_mutations_before_changing_state() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        let id = engine.notify(0, notification("Keep me", 0)).await.unwrap();
        let before = engine.active().await;
        let mut engine = Arc::try_unwrap(engine).ok().unwrap();
        engine.persistence = Some(super::NotificationPersistence::stopped_for_test());
        assert!(
            engine
                .notify(id, notification("Replacement", 0))
                .await
                .is_err()
        );
        assert!(engine.dismiss(id).await.is_err());
        assert!(engine.clear().await.is_err());
        assert!(engine.clear_group("test").await.is_err());
        assert!(
            engine
                .snooze(id, crate::time::unix_ms() + 60_000)
                .await
                .is_err()
        );
        assert!(engine.set_dnd(true, None).await.is_err());
        assert!(engine.toggle_dnd().await.is_err());
        assert!(engine.invoke_action(id, "default", None).await.is_err());
        assert!(engine.history(None, 10).await.is_err());
        assert_eq!(engine.active().await, before);
        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.notifications.count, 1);
        assert!(!snapshot.notifications.dnd);
        assert_eq!(snapshot.notification_active.notifications, before);
    }

    #[tokio::test]
    async fn sharing_suppresses_bypass_and_never_replays_messages_or_changes_dnd() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        let id = engine
            .notify(0, notification("Before sharing", 0))
            .await
            .unwrap();
        engine.observe_sharing(Some(true)).await;
        let generation = state
            .read(|s| s.notifications.sharing.as_ref().unwrap().generation)
            .await;
        engine
            .set_app_policy(
                "named:test".into(),
                super::super::policy::AppPolicy {
                    bypass_dnd: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let during = engine
            .notify(0, notification("Private message", 0))
            .await
            .unwrap();
        assert!(
            state
                .read(|s| s.notification_active.toast_decks.is_empty())
                .await
        );
        assert!(!state.read(|s| s.notifications.dnd).await);
        engine.observe_sharing(None).await;
        assert!(
            state
                .read(|s| s.notifications.sharing.as_ref().unwrap().active)
                .await
        );
        engine.snooze_sharing(generation, 0).await.unwrap();
        engine.observe_sharing(Some(false)).await;
        assert!(
            state
                .read(|s| s.notification_active.toast_decks.is_empty())
                .await,
            "No replay after sharing ends"
        );
        assert_eq!(
            engine
                .active()
                .await
                .iter()
                .map(|n| n.id)
                .collect::<Vec<_>>(),
            [id, during]
        );
        engine
            .notify(0, notification("After sharing", 0))
            .await
            .unwrap();
        assert_eq!(
            state
                .read(|s| s.notification_active.toast_decks.len())
                .await,
            1
        );
    }

    #[tokio::test]
    async fn context_actions_reject_stale_identity_and_arbitrary_keys_before_effects() {
        let engine = NotificationEngine::new(StateStore::default()).await;
        let id = engine
            .notify(0, notification("Verification code 001234", 0))
            .await
            .unwrap();
        let record = engine.active().await.remove(0);
        assert_eq!(record.context_actions[0].key, "copy-code");
        assert!(
            engine
                .context_action(
                    id,
                    record.created_unix_ms + 1,
                    record.updated_unix_ms,
                    "copy-code"
                )
                .await
                .is_err()
        );
        assert!(
            engine
                .context_action(
                    id,
                    record.created_unix_ms,
                    record.updated_unix_ms + 1,
                    "copy-code"
                )
                .await
                .is_err()
        );
        assert!(
            engine
                .context_action(
                    id,
                    record.created_unix_ms,
                    record.updated_unix_ms,
                    "javascript:bad"
                )
                .await
                .is_err()
        );
        engine.dismiss(id).await.unwrap();
        assert!(
            engine
                .context_action(
                    id,
                    record.created_unix_ms,
                    record.updated_unix_ms,
                    "copy-code"
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn popup_expiry_keeps_default_notifications_actionable_even_in_dnd() {
        use super::super::model::{NotificationAction, NotificationSignal};
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        engine.set_dnd(true, None).await.unwrap();
        let mut incoming = notification("Retained", -1);
        incoming.actions = vec![NotificationAction {
            key: "default".into(),
            label: "Open".into(),
        }];
        let id = engine.notify(0, incoming).await.unwrap();
        let mut signals = engine.subscribe_signals();
        {
            let mut data = engine.data.lock().await;
            let record = data.active.get_mut(&id).unwrap();
            assert!(record.expires_unix_ms.is_none());
            record.toast_expires_unix_ms = Some(0);
        }
        engine.expire_due().await;
        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.notifications.count, 1);
        assert!(!snapshot.notification_active.notifications[0].toast_visible);
        assert!(
            signals.try_recv().is_err(),
            "hiding a popup must not emit NotificationClosed"
        );
        assert!(
            engine
                .invoke_action(id, "default", Some("token".into()))
                .await
                .unwrap()
        );
        assert_eq!(
            signals.recv().await.unwrap(),
            NotificationSignal::ActivationToken {
                id,
                token: "token".into()
            }
        );
        assert_eq!(
            signals.recv().await.unwrap(),
            NotificationSignal::ActionInvoked {
                id,
                action_key: "default".into()
            }
        );
        assert_eq!(
            signals.recv().await.unwrap(),
            NotificationSignal::Closed {
                id,
                reason: close_reason::DISMISSED
            }
        );
        assert!(!engine.invoke_action(id, "default", None).await.unwrap());
    }
}
