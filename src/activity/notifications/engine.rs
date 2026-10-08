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
        NotificationSignal, NotificationState, close_reason,
    },
    persistence::{NotificationPersistence, PendingWrites},
    policy::NotificationPolicy,
};

struct EngineData {
    active: BTreeMap<u32, ActiveNotification>,
    dnd: bool,
    dnd_until_unix_ms: Option<u64>,
    history_revision: u64,
    app_policies: BTreeMap<String, super::policy::AppPolicy>,
    delete_tickets: BTreeMap<String, (u64, Vec<history::Position>)>,
}

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
        let stored = if let Some(existing) = self.active.get_mut(&id) {
            existing.replace_from(incoming, now);
            existing.source_monitor = source_monitor;
            existing.clone()
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
            let mut stored = ActiveNotification::from_incoming(id, incoming, now);
            stored.source_monitor = source_monitor;
            self.active.insert(id, stored.clone());
            stored
        };
        let mut stored = stored;
        let key = super::center::app_key(
            &stored.hints.desktop_entry,
            &stored.app_name,
            stored.id,
            stored.created_unix_ms,
        );
        if self
            .app_policies
            .get(&key)
            .is_some_and(|policy| policy.silenced(now))
        {
            stored.toast_visible = false;
            self.active.insert(id, stored.clone());
        }
        self.history_revision = self.history_revision.wrapping_add(1);
        (stored, evicted)
    }

    fn expire(&mut self, now: u64) -> ExpiryBatch {
        let mut changed = Vec::new();
        for policy in self.app_policies.values_mut() {
            if policy.silent && policy.until_unix_ms.is_some_and(|until| until <= now) {
                policy.silent = false;
                policy.until_unix_ms = None;
                self.history_revision = self.history_revision.wrapping_add(1);
            }
        }
        for notification in self.active.values_mut() {
            let mut dirty = false;
            if notification
                .snoozed_until_unix_ms
                .is_some_and(|until| until <= now)
            {
                notification.snoozed_until_unix_ms = None;
                dirty = true;
            }
            if notification.snoozed_until_unix_ms.is_none()
                && notification
                    .toast_expires_unix_ms
                    .is_some_and(|until| until <= now)
            {
                notification.toast_visible = false;
                notification.toast_expires_unix_ms = None;
                // Popup-only changes do not invalidate persisted history.
            }
            if dirty {
                changed.push(notification.clone());
            }
        }
        let expired_ids = self
            .active
            .values()
            .filter(|item| {
                item.snoozed_until_unix_ms.is_none()
                    && item.expires_unix_ms.is_some_and(|expiry| expiry <= now)
            })
            .map(|item| item.id)
            .collect();
        let dnd_expired = self.dnd && self.dnd_until_unix_ms.is_some_and(|until| until <= now);
        if dnd_expired {
            self.dnd = false;
            self.dnd_until_unix_ms = None;
        }
        if !changed.is_empty() {
            self.history_revision = self.history_revision.wrapping_add(1);
        }
        ExpiryBatch {
            expired_ids,
            changed,
            dnd_expired,
        }
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
}

impl NotificationEngine {
    #[cfg(test)]
    pub(crate) async fn new(state: StateStore) -> Arc<Self> {
        Self::build(state, None, 0, false, None).await.unwrap()
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
        dnd: bool,
        dnd_until_unix_ms: Option<u64>,
    ) -> Result<Arc<Self>> {
        tokio::task::spawn_blocking(super::identity::preload)
            .await
            .context("load notification application icons")?;
        let dnd_expired = dnd_until_unix_ms.is_some_and(|until| until <= unix_ms());
        let (signals, _) = broadcast::channel(256);
        let mut app_policies = if let Some(store) = &persistence {
            store.policies().await?
        } else {
            BTreeMap::new()
        };
        for policy in app_policies.values_mut() {
            if policy.until_unix_ms.is_some_and(|until| until <= unix_ms()) {
                policy.silent = false;
                policy.until_unix_ms = None;
            }
        }
        let engine = Arc::new(Self {
            mutations: Mutex::new(()),
            data: Mutex::new(EngineData {
                active: BTreeMap::new(),
                dnd: dnd && !dnd_expired,
                dnd_until_unix_ms: (!dnd_expired).then_some(dnd_until_unix_ms).flatten(),
                history_revision: 0,
                app_policies,
                delete_tickets: BTreeMap::new(),
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
                    if super::center::app_key(
                        &notification.hints.desktop_entry,
                        &notification.app_name,
                        notification.id,
                        notification.created_unix_ms,
                    ) == key
                    {
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
    ) -> Result<serde_json::Value> {
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
            if app_key.as_ref().is_none_or(|key| {
                key == &super::center::app_key(
                    &n.hints.desktop_entry,
                    &n.app_name,
                    n.id,
                    n.created_unix_ms,
                )
            }) && selected.is_none_or(|p| p.id == n.id && p.created == n.created_unix_ms)
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
        Ok(
            serde_json::json!({"token":token, "count":count, "app_key":app_key, "selected":selected, "expires_unix_ms":expires}),
        )
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

    pub(crate) async fn query_center(
        &self,
        mut query: super::center::CenterQuery,
    ) -> Result<super::center::CenterPage, HistoryError> {
        query.normalize()?;
        let _reader = self
            .history_readers
            .try_acquire()
            .map_err(|_| HistoryError::Busy)?;
        let _mutation = self.mutations.lock().await;
        let (revision, active) = {
            let data = self.data.lock().await;
            (
                data.history_revision,
                data.active
                    .values()
                    .filter(|n| n.snoozed_until_unix_ms.is_none())
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
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
        let (revision, active) = {
            let data = self.data.lock().await;
            (
                data.history_revision,
                data.active
                    .values()
                    .filter(|item| item.snoozed_until_unix_ms.is_none())
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
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
        let notification_wakeup = data.active.values().filter_map(|item| {
            item.snoozed_until_unix_ms.or_else(|| {
                item.expires_unix_ms
                    .into_iter()
                    .chain(item.toast_expires_unix_ms)
                    .min()
            })
        });
        let next = notification_wakeup
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
        let (summary, active) = {
            let data = self.data.lock().await;
            let visible = data
                .active
                .values()
                .filter(|item| item.snoozed_until_unix_ms.is_none())
                .map(|item| {
                    let mut item = item.clone();
                    let key = super::center::app_key(
                        &item.hints.desktop_entry,
                        &item.app_name,
                        item.id,
                        item.created_unix_ms,
                    );
                    if let Some(policy) = data.app_policies.get(&key) {
                        item.dnd_bypass = policy.bypass_dnd;
                        if policy.silenced(unix_ms()) {
                            item.toast_visible = false;
                            item.hints.suppress_sound = true;
                        }
                    }
                    item
                })
                .collect::<Vec<_>>();
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
                    app_policies: data.app_policies.clone(),
                    error: None,
                },
                NotificationActiveState {
                    available: true,
                    revision: data.history_revision,
                    notifications: visible,
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

    use tokio::time::{Duration, timeout};

    use crate::state::StateStore;

    use super::NotificationEngine;
    use crate::activity::notifications::model::{
        IncomingNotification, NotificationHints, close_reason,
    };

    fn notification(summary: &str, timeout_ms: i32) -> IncomingNotification {
        IncomingNotification {
            app_name: "test".into(),
            app_icon: String::new(),
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            hints: NotificationHints::default(),
            expire_timeout: timeout_ms,
        }
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

    #[tokio::test]
    async fn history_admission_is_bounded_and_releases_after_cancellation() {
        use super::history::{HistoryError, HistoryQuery};
        let engine = NotificationEngine::new(StateStore::default()).await;
        let mutation = engine.mutations.lock().await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let engine = Arc::clone(&engine);
            tasks.spawn(async move {
                engine
                    .query_history(HistoryQuery {
                        query: String::new(),
                        cursor: None,
                        anchor: None,
                        limit: 50,
                    })
                    .await
            });
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while engine.history_readers.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            engine
                .query_history(HistoryQuery {
                    query: String::new(),
                    cursor: None,
                    anchor: None,
                    limit: 50
                })
                .await,
            Err(HistoryError::Busy)
        ));
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        drop(mutation);
        assert_eq!(engine.history_readers.available_permits(), 2);
        assert!(
            engine
                .query_history(HistoryQuery {
                    query: String::new(),
                    cursor: None,
                    anchor: None,
                    limit: 50
                })
                .await
                .is_ok()
        );
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
    async fn snoozes_restores_and_clears_groups() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        let first = engine.notify(0, notification("first", 0)).await.unwrap();
        assert_eq!(
            engine
                .notify(first, notification("replacement", 0))
                .await
                .unwrap(),
            first
        );
        assert_eq!(engine.active().await[0].summary, "replacement");
        assert_eq!(state.snapshot().await.notifications.count, 1);
        engine.notify(0, notification("second", 0)).await.unwrap();
        assert!(
            engine
                .snooze(first, crate::time::unix_ms() + 10)
                .await
                .unwrap()
        );
        assert_eq!(state.snapshot().await.notifications.count, 1);

        let task = tokio::spawn(Arc::clone(&engine).run_expiry());
        timeout(Duration::from_secs(1), async {
            loop {
                if state.snapshot().await.notifications.count == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(engine.clear_group("test").await.unwrap(), 2);
        task.abort();
    }

    #[tokio::test]
    async fn batches_publish_only_final_state_and_popup_changes_do_not_dirty_history() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        engine.notify(0, notification("first", -1)).await.unwrap();
        engine.notify(0, notification("second", -1)).await.unwrap();
        let revision = state.snapshot().await.notifications.history_revision;
        for item in engine.data.lock().await.active.values_mut() {
            item.toast_expires_unix_ms = Some(0);
        }
        engine.expire_due().await;
        assert_eq!(
            state.snapshot().await.notifications.history_revision,
            revision
        );
        let mut events = state.subscribe();
        assert_eq!(engine.clear_group("test").await.unwrap(), 2);
        let mut updates = Vec::new();
        while let Ok(event) = events.try_recv() {
            updates.push(event);
        }
        assert_eq!(
            updates.len(),
            2,
            "one summary and one collection, not per-record states"
        );
        assert_eq!(updates[0].data["count"], 0);
        assert_eq!(
            updates[1].data["notifications"].as_array().unwrap().len(),
            0
        );
        let boundary = engine.mutations.lock().await;
        let history = engine.history(None, 50);
        tokio::pin!(history);
        assert!(
            futures::poll!(&mut history).is_pending(),
            "history must wait for the mutation's write-enqueue boundary"
        );
        drop(boundary);
        assert!(history.await.unwrap().is_empty());
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

    #[tokio::test]
    async fn timeout_policy_and_inline_reply_capabilities() {
        use super::super::model::{ActiveNotification, NotificationAction, NotificationSignal};
        for (requested, transient, urgency, closes, toast_times_out) in [
            (-1, false, 1, false, true),
            (-1, true, 1, true, true),
            (-1, false, 2, false, false),
            (0, false, 1, false, false),
            (10, false, 2, true, true),
            (10, true, 1, true, true),
        ] {
            let mut incoming = notification("policy", requested);
            incoming.hints.transient = transient;
            incoming.hints.urgency = urgency;
            let stored = ActiveNotification::from_incoming(1, incoming, 100);
            assert_eq!(stored.expires_unix_ms.is_some(), closes);
            assert_eq!(stored.toast_expires_unix_ms.is_some(), toast_times_out);
        }
        let engine = NotificationEngine::new(StateStore::default()).await;
        let mut incoming = notification("reply", -1);
        incoming.hints.resident = true;
        incoming.actions = vec![NotificationAction {
            key: "mail-reply-sender".into(),
            label: "Reply".into(),
        }];
        let id = engine.notify(0, incoming.clone()).await.unwrap();
        assert!(!engine.reply(id, "not inline").await);
        assert!(
            engine
                .invoke_action(id, "mail-reply-sender", None)
                .await
                .unwrap()
        );
        assert_eq!(
            engine.active().await.len(),
            1,
            "resident action remains live"
        );
        incoming.actions.push(NotificationAction {
            key: "inline-reply".into(),
            label: "Send".into(),
        });
        engine.notify(id, incoming).await.unwrap();
        let mut signals = engine.subscribe_signals();
        assert!(!engine.reply(id, "  ").await);
        assert!(engine.reply(id, "Hello").await);
        assert_eq!(
            signals.recv().await.unwrap(),
            NotificationSignal::Replied {
                id,
                text: "Hello".into()
            }
        );
        engine.dismiss(id).await.unwrap();
        assert!(!engine.reply(id, "closed").await);
    }

    #[tokio::test]
    async fn restart_archives_old_actions_and_keeps_transient_id_high_watermark() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.sqlite3");
        let engine = NotificationEngine::persistent(StateStore::default(), path.clone())
            .await
            .unwrap();
        engine
            .notify(0, notification("retained", -1))
            .await
            .unwrap();
        let mut transient = notification("private", -1);
        transient.hints.transient = true;
        let last_id = engine.notify(0, transient).await.unwrap();
        assert_eq!(engine.history(None, 100).await.unwrap().len(), 1);
        drop(engine);
        let restarted = NotificationEngine::persistent(StateStore::default(), path)
            .await
            .unwrap();
        assert!(restarted.active().await.is_empty());
        let history = restarted.history(None, 100).await.unwrap();
        assert_eq!(history[0].close_reason, Some(close_reason::UNDEFINED));
        assert!(restarted.notify(0, notification("new", -1)).await.unwrap() > last_id);
    }

    #[tokio::test]
    async fn expires_and_emits_close_reason() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        let response = engine
            .set_dnd(true, Some(crate::time::unix_ms() + 10))
            .await
            .unwrap();
        assert!(response.dnd);
        assert_eq!(response, state.read(|s| s.notifications.clone()).await);
        let mut signals = engine.subscribe_signals();
        engine.notify(0, notification("short", 5)).await.unwrap();
        let task = tokio::spawn(Arc::clone(&engine).run_expiry());
        let signal = timeout(Duration::from_secs(1), signals.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            signal,
            super::NotificationSignal::Closed {
                id: 1,
                reason: close_reason::EXPIRED
            }
        );
        timeout(Duration::from_secs(1), async {
            while state.snapshot().await.notifications.dnd {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.snapshot().await.notifications.dnd_until_unix_ms, None);
        task.abort();
    }
}
