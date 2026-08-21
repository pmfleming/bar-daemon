use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast};

use crate::{
    model::{NotificationActiveState, NotificationState},
    state::StateStore,
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
    history_revision: u64,
}

pub struct NotificationEngine {
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
    pub async fn new(state: StateStore) -> Arc<Self> {
        Self::build(state, None, Vec::new(), false).await
    }

    pub async fn persistent(state: StateStore, path: PathBuf) -> Result<Arc<Self>> {
        let (persistence, active, dnd) =
            tokio::task::spawn_blocking(move || NotificationPersistence::open(&path))
                .await
                .context("join notification database initialization")??;
        Ok(Self::build(state, Some(persistence), active, dnd).await)
    }

    async fn build(
        state: StateStore,
        persistence: Option<NotificationPersistence>,
        active: Vec<ActiveNotification>,
        dnd: bool,
    ) -> Arc<Self> {
        let next_id = active.iter().map(|item| item.id).max().unwrap_or(0);
        let active = active.into_iter().map(|item| (item.id, item)).collect();
        let (signals, _) = broadcast::channel(256);
        let engine = Arc::new(Self {
            data: Mutex::new(EngineData {
                active,
                dnd,
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
        engine.publish_summary().await;
        engine
    }

    pub fn subscribe_signals(&self) -> broadcast::Receiver<NotificationSignal> {
        self.signals.subscribe()
    }

    pub async fn notify(
        &self,
        replaces_id: u32,
        notification: IncomingNotification,
    ) -> Result<u32> {
        let _permit = Arc::clone(&self.ingress)
            .try_acquire_owned()
            .context("notification ingress is full")?;
        self.policy.validate(&notification)?;
        let now = unix_ms();
        let mut evicted = None;
        let (id, stored) = {
            let mut data = self.data.lock().await;
            let id = if replaces_id != 0 && data.active.contains_key(&replaces_id) {
                replaces_id
            } else {
                self.allocate_id(&data.active)?
            };
            if let Some(existing) = data.active.get_mut(&id) {
                existing.replace_from(notification, now);
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
                data.active
                    .insert(id, ActiveNotification::from_incoming(id, notification, now));
            }
            data.history_revision = data.history_revision.wrapping_add(1);
            (
                id,
                data.active.get(&id).expect("inserted notification").clone(),
            )
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

    pub async fn close(&self, id: u32, reason: u32) -> bool {
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

    pub async fn dismiss(&self, id: u32) -> bool {
        self.close(id, close_reason::DISMISSED).await
    }

    pub async fn clear(&self) -> usize {
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

    pub async fn set_dnd(&self, enabled: bool) {
        let changed = {
            let mut data = self.data.lock().await;
            let changed = data.dnd != enabled;
            data.dnd = enabled;
            changed
        };
        if changed {
            if let Some(persistence) = &self.persistence {
                persistence.set_dnd(enabled);
            }
            self.publish_summary().await;
        }
    }

    pub async fn toggle_dnd(&self) -> bool {
        let enabled = {
            let mut data = self.data.lock().await;
            data.dnd = !data.dnd;
            data.dnd
        };
        if let Some(persistence) = &self.persistence {
            persistence.set_dnd(enabled);
        }
        self.publish_summary().await;
        enabled
    }

    pub async fn dnd(&self) -> bool {
        self.data.lock().await.dnd
    }

    pub async fn active(&self) -> Vec<ActiveNotification> {
        self.data.lock().await.active.values().cloned().collect()
    }

    pub async fn history(
        &self,
        before_history_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<HistoryNotification>> {
        let Some(persistence) = &self.persistence else {
            return Ok(Vec::new());
        };
        persistence.list(before_history_id, limit).await
    }

    pub async fn visible(&self) -> Vec<ActiveNotification> {
        let data = self.data.lock().await;
        if data.dnd {
            Vec::new()
        } else {
            data.active.values().cloned().collect()
        }
    }

    pub async fn reply(&self, id: u32, text: &str) -> bool {
        if text.is_empty() || !self.data.lock().await.active.contains_key(&id) {
            return false;
        }
        self.emit(NotificationSignal::Replied {
            id,
            text: text.into(),
        });
        true
    }

    pub async fn invoke_action(&self, id: u32, action_key: &str, token: Option<String>) -> bool {
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

    pub async fn run_expiry(self: Arc<Self>) {
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
        let next = self
            .data
            .lock()
            .await
            .active
            .values()
            .filter_map(|item| item.expires_unix_ms)
            .min()?;
        Some(Duration::from_millis(next.saturating_sub(unix_ms())))
    }

    async fn expire_due(&self) {
        let now = unix_ms();
        let ids = {
            let data = self.data.lock().await;
            data.active
                .values()
                .filter(|item| item.expires_unix_ms.is_some_and(|expiry| expiry <= now))
                .map(|item| item.id)
                .collect::<Vec<_>>()
        };
        for id in ids {
            self.close(id, close_reason::EXPIRED).await;
        }
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
            let count = data.active.len().try_into().unwrap_or(u32::MAX);
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
                    notifications: data.active.values().cloned().collect(),
                    error: None,
                },
            )
        };
        self.state.update_notifications(summary).await;
        self.state.update_notification_active(active).await;
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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
