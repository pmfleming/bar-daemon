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

use crate::{
    model::{NotificationActiveState, NotificationState},
    state::StateStore,
    time::unix_ms,
};

use super::{
    model::{
        ActiveNotification, HistoryNotification, IncomingNotification, NotificationSignal,
        close_reason,
    },
    persistence::NotificationPersistence,
    policy::NotificationPolicy,
};

struct EngineData {
    active: BTreeMap<u32, ActiveNotification>,
    dnd: bool,
    dnd_until_unix_ms: Option<u64>,
    history_revision: u64,
}

pub(crate) struct NotificationEngine {
    data: Mutex<EngineData>,
    next_id: AtomicU32,
    ingress: Arc<Semaphore>,
    expiry_wakeup: Notify,
    signals: broadcast::Sender<NotificationSignal>,
    state: StateStore,
    policy: NotificationPolicy,
    persistence: Option<NotificationPersistence>,
}

impl NotificationEngine {
    #[cfg(test)]
    pub(crate) async fn new(state: StateStore) -> Arc<Self> {
        Self::build(state, None, Vec::new(), false, None).await
    }

    pub(crate) async fn persistent(state: StateStore, path: PathBuf) -> Result<Arc<Self>> {
        let (persistence, active, dnd, dnd_until_unix_ms) =
            tokio::task::spawn_blocking(move || NotificationPersistence::open(&path))
                .await
                .context("join notification database initialization")??;
        Ok(Self::build(state, Some(persistence), active, dnd, dnd_until_unix_ms).await)
    }

    async fn build(
        state: StateStore,
        persistence: Option<NotificationPersistence>,
        active: Vec<ActiveNotification>,
        dnd: bool,
        dnd_until_unix_ms: Option<u64>,
    ) -> Arc<Self> {
        let next_id = active.iter().map(|item| item.id).max().unwrap_or(0);
        let active = active.into_iter().map(|item| (item.id, item)).collect();
        let dnd_expired = dnd_until_unix_ms.is_some_and(|until| until <= unix_ms());
        let (signals, _) = broadcast::channel(256);
        let engine = Arc::new(Self {
            data: Mutex::new(EngineData {
                active,
                dnd: dnd && !dnd_expired,
                dnd_until_unix_ms: (!dnd_expired).then_some(dnd_until_unix_ms).flatten(),
                history_revision: 0,
            }),
            next_id: AtomicU32::new(next_id),
            ingress: Arc::new(Semaphore::new(256)),
            expiry_wakeup: Notify::new(),
            signals,
            state,
            policy: NotificationPolicy::default(),
            persistence,
        });
        if dnd_expired && let Some(persistence) = &engine.persistence {
            persistence.set_dnd(false, None);
        }
        engine.publish_summary().await;
        engine
    }

    pub(crate) fn subscribe_signals(&self) -> broadcast::Receiver<NotificationSignal> {
        self.signals.subscribe()
    }

    pub(crate) async fn notify(
        &self,
        replaces_id: u32,
        notification: IncomingNotification,
    ) -> Result<u32> {
        let _permit = Arc::clone(&self.ingress)
            .try_acquire_owned()
            .context("notification ingress is full")?;
        self.policy.validate(&notification)?;
        let now = unix_ms();
        let source_monitor = self
            .state
            .snapshot()
            .await
            .workspaces
            .focused_monitor
            .unwrap_or_default();
        let mut evicted = None;
        let (id, stored) = {
            let mut data = self.data.lock().await;
            let id = if replaces_id != 0 && data.active.contains_key(&replaces_id) {
                replaces_id
            } else {
                self.allocate_id(&data.active)?
            };
            let stored = if let Some(existing) = data.active.get_mut(&id) {
                existing.replace_from(notification, now);
                existing.source_monitor.clone_from(&source_monitor);
                existing.clone()
            } else {
                if data.active.len() >= self.policy.maximum_active
                    && let Some(oldest) = data
                        .active
                        .values()
                        .min_by_key(|item| (item.created_unix_ms, item.id))
                        .map(|item| item.id)
                {
                    data.active.remove(&oldest);
                    evicted = Some(oldest);
                }
                let mut stored = ActiveNotification::from_incoming(id, notification, now);
                stored.source_monitor.clone_from(&source_monitor);
                data.active.insert(id, stored.clone());
                stored
            };
            data.history_revision = data.history_revision.wrapping_add(1);
            (id, stored)
        };
        if let Some(persistence) = &self.persistence {
            persistence.save(stored);
        }
        if let Some(id) = evicted {
            self.emit(NotificationSignal::Closed {
                id,
                reason: close_reason::UNDEFINED,
            });
            if let Some(persistence) = &self.persistence {
                persistence.close(id, now, close_reason::UNDEFINED);
            }
        }
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        Ok(id)
    }

    pub(crate) async fn close(&self, id: u32, reason: u32) -> bool {
        let removed = {
            let mut data = self.data.lock().await;
            let removed = data.active.remove(&id).is_some();
            if removed {
                data.history_revision = data.history_revision.wrapping_add(1);
            }
            removed
        };
        if removed {
            if let Some(persistence) = &self.persistence {
                persistence.close(id, unix_ms(), reason);
            }
            self.emit(NotificationSignal::Closed { id, reason });
            self.expiry_wakeup.notify_one();
            self.publish_summary().await;
        }
        removed
    }

    pub(crate) async fn dismiss(&self, id: u32) -> bool {
        self.close(id, close_reason::DISMISSED).await
    }

    pub(crate) async fn clear(&self) -> usize {
        let ids = {
            let mut data = self.data.lock().await;
            let ids = data.active.keys().copied().collect::<Vec<_>>();
            data.active.clear();
            if !ids.is_empty() {
                data.history_revision = data.history_revision.wrapping_add(1);
            }
            ids
        };
        if let Some(persistence) = &self.persistence
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
        ids.len()
    }

    pub(crate) async fn set_dnd(&self, enabled: bool, until_unix_ms: Option<u64>) {
        let until = enabled.then_some(until_unix_ms).flatten();
        let changed = {
            let mut data = self.data.lock().await;
            let changed = data.dnd != enabled || data.dnd_until_unix_ms != until;
            data.dnd = enabled;
            data.dnd_until_unix_ms = until;
            changed
        };
        if changed {
            if let Some(persistence) = &self.persistence {
                persistence.set_dnd(enabled, until);
            }
            self.expiry_wakeup.notify_one();
            self.publish_summary().await;
        }
    }

    pub(crate) async fn toggle_dnd(&self) -> bool {
        let enabled = !self.data.lock().await.dnd;
        self.set_dnd(enabled, None).await;
        enabled
    }

    pub(crate) async fn snooze(&self, id: u32, until_unix_ms: u64) -> bool {
        let now = unix_ms();
        if until_unix_ms <= now {
            return false;
        }
        let stored = {
            let mut data = self.data.lock().await;
            let Some(notification) = data.active.get_mut(&id) else {
                return false;
            };
            notification.snoozed_until_unix_ms = Some(until_unix_ms);
            notification.updated_unix_ms = now;
            if notification.expires_unix_ms.is_some() {
                notification.expires_unix_ms = Some(until_unix_ms.saturating_add(5_000));
            }
            let stored = notification.clone();
            data.history_revision = data.history_revision.wrapping_add(1);
            stored
        };
        if let Some(persistence) = &self.persistence {
            persistence.save(stored);
        }
        self.expiry_wakeup.notify_one();
        self.publish_summary().await;
        true
    }

    pub(crate) async fn clear_group(&self, group_key: &str) -> usize {
        let ids = {
            let data = self.data.lock().await;
            data.active
                .values()
                .filter(|item| item.group_key == group_key)
                .map(|item| item.id)
                .collect::<Vec<_>>()
        };
        for id in &ids {
            self.close(*id, close_reason::DISMISSED).await;
        }
        ids.len()
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
        let Some(persistence) = &self.persistence else {
            return Ok(Vec::new());
        };
        persistence.list(before_history_id, limit).await
    }

    pub(crate) async fn reply(&self, id: u32, text: &str) -> bool {
        if text.is_empty() || !self.data.lock().await.active.contains_key(&id) {
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
    ) -> bool {
        let resident = {
            let data = self.data.lock().await;
            let Some(notification) = data.active.get(&id) else {
                return false;
            };
            if !notification
                .actions
                .iter()
                .any(|action| action.key == action_key)
            {
                return false;
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
            self.dismiss(id).await;
        }
        true
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
        let notification_wakeup = data
            .active
            .values()
            .filter_map(|item| item.snoozed_until_unix_ms.or(item.expires_unix_ms));
        let next = notification_wakeup.chain(data.dnd_until_unix_ms).min()?;
        Some(Duration::from_millis(next.saturating_sub(unix_ms())))
    }

    async fn expire_due(&self) {
        let now = unix_ms();
        let (ids, awakened, dnd_expired) = {
            let mut data = self.data.lock().await;
            let mut awakened = Vec::new();
            for notification in data.active.values_mut() {
                if notification
                    .snoozed_until_unix_ms
                    .is_some_and(|until| until <= now)
                {
                    notification.snoozed_until_unix_ms = None;
                    awakened.push(notification.clone());
                }
            }
            let ids = data
                .active
                .values()
                .filter(|item| {
                    item.snoozed_until_unix_ms.is_none()
                        && item.expires_unix_ms.is_some_and(|expiry| expiry <= now)
                })
                .map(|item| item.id)
                .collect::<Vec<_>>();
            let dnd_expired = data.dnd && data.dnd_until_unix_ms.is_some_and(|until| until <= now);
            if dnd_expired {
                data.dnd = false;
                data.dnd_until_unix_ms = None;
            }
            if !awakened.is_empty() || dnd_expired {
                data.history_revision = data.history_revision.wrapping_add(1);
            }
            (ids, awakened, dnd_expired)
        };
        if let Some(persistence) = &self.persistence {
            for notification in awakened {
                persistence.save(notification);
            }
            if dnd_expired {
                persistence.set_dnd(false, None);
            }
        }
        for id in ids {
            self.close(id, close_reason::EXPIRED).await;
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
                .cloned()
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

    #[tokio::test]
    async fn allocates_replaces_and_updates_summary() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        let first = engine.notify(0, notification("first", 0)).await.unwrap();
        let replaced = engine
            .notify(first, notification("replacement", 0))
            .await
            .unwrap();
        assert_eq!(first, replaced);
        assert_eq!(engine.active().await[0].summary, "replacement");
        assert_eq!(state.snapshot().await.notifications.count, 1);
    }

    #[tokio::test]
    async fn snoozes_restores_and_clears_groups() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        let first = engine.notify(0, notification("first", 0)).await.unwrap();
        engine.notify(0, notification("second", 0)).await.unwrap();
        assert!(engine.snooze(first, crate::time::unix_ms() + 10).await);
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
        assert_eq!(engine.clear_group("test").await, 2);
        task.abort();
    }

    #[tokio::test]
    async fn timed_dnd_expires() {
        let state = StateStore::default();
        let engine = NotificationEngine::new(state.clone()).await;
        engine
            .set_dnd(true, Some(crate::time::unix_ms() + 10))
            .await;
        let task = tokio::spawn(Arc::clone(&engine).run_expiry());
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

    #[tokio::test]
    async fn expires_and_emits_close_reason() {
        let engine = NotificationEngine::new(StateStore::default()).await;
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
        task.abort();
    }
}
