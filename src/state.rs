use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, to_value};
use tokio::sync::{RwLock, broadcast};

use crate::model::{
    ActivityState, AudioState, BarSnapshot, BatteryState, BrightnessState, MediaState,
    NotificationActiveState, NotificationState, OsdHardwareState, PowerProfileState,
    PowerSleepState, TimezoneState, UpdateState, WorkspaceState,
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
    work_area_demand: tokio::sync::watch::Sender<usize>,
    pub(crate) work_area_changed: Arc<tokio::sync::Notify>,
}

impl Default for StateStore {
    fn default() -> Self {
        let (events, _) = broadcast::channel(128);
        Self {
            snapshot: Arc::new(RwLock::new(BarSnapshot::default())),
            events,
            work_area_demand: tokio::sync::watch::channel(0).0,
            work_area_changed: Arc::new(tokio::sync::Notify::new()),
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
        update_sleep_policy(crate::sleep_policy::SleepPolicyState) => sleep_policy, crate::protocol::stream::SLEEP_POLICY;
        update_osd_hardware(OsdHardwareState) => osd_hardware, crate::protocol::stream::OSD_HARDWARE;
        update_battery(BatteryState) => battery, crate::protocol::stream::BATTERY;
        update_brightness(BrightnessState) => brightness, crate::protocol::stream::BRIGHTNESS;
        update_audio(AudioState) => audio, crate::protocol::stream::AUDIO;
        update_media(MediaState) => media, crate::protocol::stream::MEDIA;
    }

    pub(crate) fn work_area_interest(&self) -> crate::work_area::Interest {
        crate::work_area::Interest::new(self.work_area_demand.clone())
    }
    pub(crate) fn work_area_demand(&self) -> tokio::sync::watch::Receiver<usize> {
        self.work_area_demand.subscribe()
    }
    pub(crate) async fn update_work_area(&self, mut value: crate::work_area::WorkAreaState) {
        let mut snapshot = self.snapshot.write().await;
        value.revision = snapshot.workarea.revision;
        if value == snapshot.workarea {
            return;
        }
        value.revision = value.revision.saturating_add(1);
        let data = to_value(&value).unwrap_or(Value::Null);
        snapshot.workarea = value;
        let _ = self.events.send(DomainEvent {
            stream: crate::protocol::stream::WORKAREA.into(),
            data,
        });
    }

    pub(crate) async fn update_power_sleep(&self, value: PowerSleepState) {
        self.commit_power_sleep(Some(value), false).await;
    }

    pub(crate) async fn record_resume(&self) {
        self.commit_power_sleep(None, true).await;
    }

    async fn commit_power_sleep(&self, value: Option<PowerSleepState>, resumed: bool) {
        let mut snapshot = self.snapshot.write().await;
        let current = &mut snapshot.power_sleep;
        let mut next = value.unwrap_or_else(|| current.clone());
        next.resume_generation = current.resume_generation.saturating_add(u64::from(resumed));
        if resumed {
            next.preparing_for_sleep = false;
        }
        if *current == next {
            return;
        }
        let data = to_value(&next).unwrap_or(Value::Null);
        *current = next;
        let _ = self.events.send(DomainEvent {
            stream: crate::protocol::stream::POWER_SLEEP.into(),
            data,
        });
    }

    async fn update<T, F>(&self, value: T, stream: &str, field: F)
    where
        T: PartialEq + Serialize,
        F: for<'a> FnOnce(&'a mut BarSnapshot) -> &'a mut T,
    {
        let mut snapshot = self.snapshot.write().await;
        let current = field(&mut snapshot);
        if *current == value {
            return;
        }
        let data = to_value(&value).unwrap_or(Value::Null);
        *current = value;
        // Commit and broadcast share the snapshot/subscription boundary. Sending
        // is synchronous, so retaining the lock also preserves commit order.
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_events_follow_commit_order() {
        use std::sync::{Arc, Mutex};

        let store = StateStore::default();
        let mut events = store.subscribe();
        for round in 0..1_000 {
            let committed = Arc::new(Mutex::new(Vec::new()));
            let mut tasks = tokio::task::JoinSet::new();
            for worker in 0..16 {
                let store = store.clone();
                let committed = Arc::clone(&committed);
                tasks.spawn(async move {
                    let value = round * 16 + worker + 1;
                    store
                        .update(value, "test", |snapshot| {
                            committed.lock().unwrap().push(value);
                            &mut snapshot.notifications.count
                        })
                        .await;
                });
            }
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
            let mut emitted = Vec::new();
            while let Ok(event) = events.try_recv() {
                emitted.push(event.data.as_u64().unwrap() as u32);
            }
            assert_eq!(emitted, *committed.lock().unwrap());
            assert_eq!(
                emitted.last(),
                Some(&store.snapshot().await.notifications.count)
            );
            let (snapshot, mut new_events) = store.snapshot_and_subscribe().await;
            assert_eq!(snapshot.notifications.count, *emitted.last().unwrap());
            assert!(new_events.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn resume_generation_survives_stale_action_and_monitor_telemetry() {
        let store = StateStore::default();
        let mut events = store.subscribe();
        store.record_resume().await;
        assert_eq!(events.recv().await.unwrap().data["resume_generation"], 1);
        store
            .update_power_sleep(crate::model::PowerSleepState {
                available: true,
                ..Default::default()
            })
            .await;
        assert_eq!(events.recv().await.unwrap().data["resume_generation"], 1);
        store.record_resume().await;
        assert_eq!(store.snapshot().await.power_sleep.resume_generation, 2);
    }

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
