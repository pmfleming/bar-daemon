use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, to_value};
use tokio::sync::{RwLock, broadcast};

use crate::model::{
    ActivityState, AudioState, BarSnapshot, BatteryState, BrightnessState, MediaState,
    NotificationActiveState, NotificationState, OsdHardwareState, PowerProfileState,
    PowerSleepState, SleepOperation, TimezoneState, UpdateState, WorkspaceState,
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
    /// Read a projection under the snapshot lock without cloning unrelated domains.
    pub(crate) async fn read<T>(&self, project: impl FnOnce(&BarSnapshot) -> T) -> T {
        let snapshot = self.snapshot.read().await;
        project(&snapshot)
    }

    pub(crate) async fn snapshot(&self) -> BarSnapshot {
        self.snapshot.read().await.clone()
    }

    /// Project only requested domains while preserving snapshot/event commit order.
    pub(crate) async fn snapshot_and_subscribe<T>(
        &self,
        project: impl FnOnce(&BarSnapshot) -> T,
    ) -> (T, broadcast::Receiver<DomainEvent>) {
        let snapshot = self.snapshot.read().await;
        let events = self.events.subscribe();
        (project(&snapshot), events)
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
        update_display_policy(crate::display_policy::DisplayPolicyState) => display_policy, crate::protocol::stream::DISPLAY_POLICY;
        update_osd_hardware(OsdHardwareState) => osd_hardware, crate::protocol::stream::OSD_HARDWARE;
        update_battery(BatteryState) => battery, crate::protocol::stream::BATTERY;
        update_brightness(BrightnessState) => brightness, crate::protocol::stream::BRIGHTNESS;
        update_audio(AudioState) => audio, crate::protocol::stream::AUDIO;
        update_media(MediaState) => media, crate::protocol::stream::MEDIA;
    }

    /// Lid ownership is independently observed; policy I/O must not overwrite it.
    pub(crate) async fn update_sleep_policy(
        &self,
        mut value: crate::sleep_policy::SleepPolicyState,
    ) {
        let mut snapshot = self.snapshot.write().await;
        value.lid = snapshot.sleep_policy.lid.clone();
        value.critical_battery = snapshot.sleep_policy.critical_battery.clone();
        if snapshot.sleep_policy != value {
            snapshot.sleep_policy = value;
            self.publish(
                crate::protocol::stream::SLEEP_POLICY,
                &snapshot.sleep_policy,
            );
        }
    }

    pub(crate) async fn update_critical_battery(
        &self,
        state: crate::sleep_policy::critical::State,
    ) {
        self.update_policy_part(state, |s| &mut s.critical_battery)
            .await;
    }

    pub(crate) async fn update_lid(&self, lid: crate::sleep_policy::lid::LidState) {
        self.update_policy_part(lid, |s| &mut s.lid).await;
    }

    async fn update_policy_part<T: PartialEq>(
        &self,
        value: T,
        field: impl FnOnce(&mut crate::sleep_policy::SleepPolicyState) -> &mut T,
    ) {
        let mut snapshot = self.snapshot.write().await;
        let current = field(&mut snapshot.sleep_policy);
        if *current != value {
            *current = value;
            self.publish(
                crate::protocol::stream::SLEEP_POLICY,
                &snapshot.sleep_policy,
            );
        }
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
        snapshot.workarea = value;
        self.publish(crate::protocol::stream::WORKAREA, &snapshot.workarea);
    }

    pub(crate) async fn update_power_sleep(&self, value: PowerSleepState) {
        self.commit_power_sleep(Some(value), false, None, None)
            .await;
    }

    pub(crate) async fn update_power_sleep_if_unchanged(
        &self,
        value: PowerSleepState,
        expected: &PowerSleepState,
    ) -> bool {
        self.commit_power_sleep(Some(value), false, None, Some(expected))
            .await
    }

    pub(crate) async fn record_sleep_operation(&self, operation: SleepOperation) {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.power_sleep.operation == operation {
            return;
        }
        snapshot.power_sleep.operation = operation;
        self.publish(crate::protocol::stream::POWER_SLEEP, &snapshot.power_sleep);
    }

    pub(crate) async fn record_resume(&self) {
        self.commit_power_sleep(None, true, Some(false), None).await;
    }

    pub(crate) async fn record_sleep_preparation(&self, preparing: bool) {
        self.commit_power_sleep(None, false, Some(preparing), None)
            .await;
    }

    async fn commit_power_sleep(
        &self,
        value: Option<PowerSleepState>,
        resumed: bool,
        preparing: Option<bool>,
        expected: Option<&PowerSleepState>,
    ) -> bool {
        let mut snapshot = self.snapshot.write().await;
        let current = &mut snapshot.power_sleep;
        if expected.is_some_and(|expected| expected != current) {
            return false;
        }
        let mut next = value.unwrap_or_else(|| current.clone());
        next.resume_generation = current.resume_generation.saturating_add(u64::from(resumed));
        next.operation = current.operation.clone();
        if let Some(preparing) = preparing {
            next.preparing_for_sleep = preparing;
        }
        if *current == next {
            return true;
        }
        *current = next;
        self.publish(crate::protocol::stream::POWER_SLEEP, current);
        true
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
        *current = value;
        self.publish(stream, current);
    }

    // Called under the snapshot write lock: subscription and commit order agree.
    fn publish(&self, stream: &str, value: &impl Serialize) {
        let _ = self.events.send(DomainEvent {
            stream: stream.into(),
            data: to_value(value).unwrap_or(Value::Null),
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::StateStore;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_events_follow_commit_order() {
        use std::sync::{Arc, Mutex};

        let store = StateStore::default();
        let (initial, mut events) = store
            .snapshot_and_subscribe(|s| s.notifications.count)
            .await;
        assert_eq!(initial, 0);
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
            let (count, mut new_events) = store
                .snapshot_and_subscribe(|s| s.notifications.count)
                .await;
            assert_eq!(count, *emitted.last().unwrap());
            store
                .update(count, "test", |s| &mut s.notifications.count)
                .await;
            assert!(
                new_events.try_recv().is_err(),
                "unchanged values must not emit events"
            );
        }
    }

    #[tokio::test]
    async fn sleep_policy_refresh_cannot_overwrite_independent_lid_ownership() {
        let store = StateStore::default();
        let stale = store.snapshot().await.sleep_policy;
        let lid = crate::sleep_policy::lid::LidState {
            available: true,
            managed: false,
            error: Some("session inactive".into()),
        };
        timeout(Duration::from_millis(50), store.update_lid(lid.clone()))
            .await
            .unwrap();
        store.update_sleep_policy(stale).await;
        assert_eq!(store.snapshot().await.sleep_policy.lid, lid);
    }

    #[tokio::test]
    async fn stale_action_telemetry_cannot_clear_a_late_sleep_job_failure() {
        let store = StateStore::default();
        let stale = store.snapshot().await.power_sleep;
        let operation = crate::model::SleepOperation {
            id: 3,
            action: "hibernate".into(),
            phase: "failed".into(),
            job: Some("/job/3".into()),
            error: Some("hibernate service failed".into()),
        };
        store.record_sleep_operation(operation.clone()).await;
        store.update_power_sleep(stale).await;
        assert_eq!(store.snapshot().await.power_sleep.operation, operation);
    }

    #[tokio::test]
    async fn slow_telemetry_cannot_overwrite_newer_sleep_state() {
        let store = StateStore::default();
        let before = store.snapshot().await.power_sleep;
        store.record_sleep_preparation(true).await;
        assert!(
            !store
                .update_power_sleep_if_unchanged(before.clone(), &before)
                .await
        );
        assert!(store.snapshot().await.power_sleep.preparing_for_sleep);
        let asleep = store.snapshot().await.power_sleep;
        store.record_resume().await;
        assert!(
            !store
                .update_power_sleep_if_unchanged(asleep.clone(), &asleep)
                .await
        );
        let resumed = store.snapshot().await.power_sleep;
        assert!(!resumed.preparing_for_sleep);
        assert_eq!(resumed.resume_generation, 1);
        let mut inhibited = resumed.clone();
        inhibited.keep_awake = true;
        store.update_power_sleep(inhibited.clone()).await;
        assert!(
            !store
                .update_power_sleep_if_unchanged(resumed.clone(), &resumed)
                .await
        );
        assert!(
            store
                .update_power_sleep_if_unchanged(inhibited.clone(), &inhibited)
                .await
        );
        assert_eq!(store.snapshot().await.power_sleep, inhibited);
        store
            .update_power_sleep(crate::model::PowerSleepState::default())
            .await;
        assert_eq!(store.snapshot().await.power_sleep.resume_generation, 1);
        store.record_resume().await;
        assert_eq!(store.snapshot().await.power_sleep.resume_generation, 2);
    }
}
