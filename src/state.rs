use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, to_value};
use tokio::sync::{RwLock, broadcast};

use crate::model::{
    ActivityState, AudioState, BarSnapshot, BatteryState, BrightnessState, MediaState,
    NotificationActiveState, NotificationState, PowerProfileState, PowerSleepState, TimezoneState,
    UpdateState, WorkspaceState,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DomainEvent {
    pub stream: String,
    pub data: Value,
}

#[derive(Clone)]
pub struct StateStore {
    snapshot: Arc<RwLock<BarSnapshot>>,
    events: broadcast::Sender<DomainEvent>,
}

impl Default for StateStore {
    fn default() -> Self {
        let (events, _) = broadcast::channel(128);
        Self {
            snapshot: Arc::new(RwLock::new(BarSnapshot::default())),
            events,
        }
    }
}

impl StateStore {
    pub async fn snapshot(&self) -> BarSnapshot {
        self.snapshot.read().await.clone()
    }

    pub async fn snapshot_and_subscribe(&self) -> (BarSnapshot, broadcast::Receiver<DomainEvent>) {
        let snapshot = self.snapshot.read().await;
        let events = self.events.subscribe();
        (snapshot.clone(), events)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.events.subscribe()
    }

    pub async fn update_activity(&self, value: ActivityState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.activity == value {
            return;
        }
        snapshot.activity = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::ACTIVITY, value);
    }

    pub async fn update_workspaces(&self, value: WorkspaceState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.workspaces == value {
            return;
        }
        snapshot.workspaces = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::WORKSPACES, value);
    }

    pub async fn update_timezone(&self, value: TimezoneState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.timezone == value {
            return;
        }
        snapshot.timezone = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::TIMEZONE, value);
    }

    pub async fn update_updates(&self, value: UpdateState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.updates == value {
            return;
        }
        snapshot.updates = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::UPDATES, value);
    }

    pub async fn update_notification_active(&self, value: NotificationActiveState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.notification_active == value {
            return;
        }
        snapshot.notification_active = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::NOTIFICATION_ACTIVE, value);
    }

    pub async fn update_notifications(&self, value: NotificationState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.notifications == value {
            return;
        }
        snapshot.notifications = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::NOTIFICATIONS, value);
    }

    pub async fn update_power_profile(&self, value: PowerProfileState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.power_profile == value {
            return;
        }
        snapshot.power_profile = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::POWER_PROFILE, value);
    }

    pub async fn update_power_sleep(&self, value: PowerSleepState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.power_sleep == value {
            return;
        }
        snapshot.power_sleep = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::POWER_SLEEP, value);
    }

    pub async fn update_battery(&self, value: BatteryState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.battery == value {
            return;
        }
        snapshot.battery = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::BATTERY, value);
    }

    pub async fn update_brightness(&self, value: BrightnessState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.brightness == value {
            return;
        }
        snapshot.brightness = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::BRIGHTNESS, value);
    }

    pub async fn update_audio(&self, value: AudioState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.audio == value {
            return;
        }
        snapshot.audio = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::AUDIO, value);
    }

    pub async fn update_media(&self, value: MediaState) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.media == value {
            return;
        }
        snapshot.media = value.clone();
        drop(snapshot);
        self.emit(crate::protocol::stream::MEDIA, value);
    }

    fn emit(&self, stream: &str, value: impl Serialize) {
        let _ = self.events.send(DomainEvent {
            stream: stream.to_string(),
            data: to_value(value).unwrap_or(Value::Null),
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use crate::model::WorkspaceState;

    use super::StateStore;

    #[tokio::test]
    async fn only_emits_changed_domain_values() {
        let store = StateStore::default();
        let mut events = store.subscribe();
        let state = WorkspaceState {
            available: true,
            ..WorkspaceState::default()
        };
        store.update_workspaces(state.clone()).await;
        assert_eq!(events.recv().await.unwrap().data["available"], true);
        store.update_workspaces(state).await;
        assert!(
            timeout(Duration::from_millis(10), events.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn snapshot_and_subscription_share_one_update_boundary() {
        let store = StateStore::default();
        let (snapshot, mut events) = store.snapshot_and_subscribe().await;
        assert!(!snapshot.workspaces.available);
        store
            .update_workspaces(WorkspaceState {
                available: true,
                ..WorkspaceState::default()
            })
            .await;
        assert_eq!(events.recv().await.unwrap().data["available"], true);
    }
}
