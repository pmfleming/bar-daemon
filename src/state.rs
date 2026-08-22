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
pub(crate) struct DomainEvent {
    pub stream: String,
    pub data: Value,
}

#[derive(Clone)]
pub(crate) struct StateStore {
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

macro_rules! state_updates {
    ($($method:ident($state:ty) => $field:ident, $stream:expr;)*) => {
        $(
            pub(crate) async fn $method(&self, value: $state) {
                self.update(value, $stream, |snapshot| &mut snapshot.$field).await;
            }
        )*
    };
}

impl StateStore {
    pub(crate) async fn snapshot(&self) -> BarSnapshot {
        self.snapshot.read().await.clone()
    }

    pub(crate) async fn snapshot_and_subscribe(
        &self,
    ) -> (BarSnapshot, broadcast::Receiver<DomainEvent>) {
        let snapshot = self.snapshot.read().await;
        let events = self.events.subscribe();
        (snapshot.clone(), events)
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.events.subscribe()
    }

    state_updates! {
        update_activity(ActivityState) => activity, crate::protocol::stream::ACTIVITY;
        update_workspaces(WorkspaceState) => workspaces, crate::protocol::stream::WORKSPACES;
        update_timezone(TimezoneState) => timezone, crate::protocol::stream::TIMEZONE;
        update_updates(UpdateState) => updates, crate::protocol::stream::UPDATES;
        update_notification_active(NotificationActiveState) => notification_active, crate::protocol::stream::NOTIFICATION_ACTIVE;
        update_notifications(NotificationState) => notifications, crate::protocol::stream::NOTIFICATIONS;
        update_power_profile(PowerProfileState) => power_profile, crate::protocol::stream::POWER_PROFILE;
        update_power_sleep(PowerSleepState) => power_sleep, crate::protocol::stream::POWER_SLEEP;
        update_battery(BatteryState) => battery, crate::protocol::stream::BATTERY;
        update_brightness(BrightnessState) => brightness, crate::protocol::stream::BRIGHTNESS;
        update_audio(AudioState) => audio, crate::protocol::stream::AUDIO;
        update_media(MediaState) => media, crate::protocol::stream::MEDIA;
    }

    async fn update<T, F>(&self, value: T, stream: &str, field: F)
    where
        T: PartialEq + Serialize,
        F: for<'a> FnOnce(&'a mut BarSnapshot) -> &'a mut T,
    {
        let data;
        {
            let mut snapshot = self.snapshot.write().await;
            let current = field(&mut snapshot);
            if *current == value {
                return;
            }
            data = to_value(&value).unwrap_or(Value::Null);
            *current = value;
        }
        let _ = self.events.send(DomainEvent {
            stream: stream.to_string(),
            data,
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
