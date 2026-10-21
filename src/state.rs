use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use serde_json::{Value, to_value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::RwLock;

mod events;
pub(crate) use events::DomainReceivers;
use events::Source;

use crate::activity::notifications::model::{NotificationActiveState, NotificationState};
use crate::model::{
    ActivityState, AudioState, BarSnapshot, BatteryState, BrightnessState, CompositorState,
    MediaState, OsdHardwareState, PowerProfileState, PowerSleepState, SleepOperation,
    TimezoneState, UpdateState, WorkAreaState, WorkspaceState,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DomainEvent {
    pub stream: String,
    pub data: Value,
}

/// Subscription-scoped demand belongs to the store, not the geometry worker.
/// Dropping the last token lets an in-flight geometry read be cancelled.
pub(crate) struct WorkAreaInterest(tokio::sync::watch::Sender<usize>);
impl Drop for WorkAreaInterest {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count = count.saturating_sub(1));
    }
}

#[derive(Clone)]
pub(crate) struct StateStore {
    snapshot: Arc<RwLock<BarSnapshot>>,
    events: Arc<BTreeMap<&'static str, Source>>,
    failed_monitors: Arc<Mutex<BTreeMap<&'static str, String>>>,
    work_area_demand: tokio::sync::watch::Sender<usize>,
    hyprland_connected: Arc<AtomicBool>,
    pub(crate) work_area_changed: Arc<tokio::sync::Notify>,
    pub(crate) compositor_changed: Arc<tokio::sync::Notify>,
}

impl Default for StateStore {
    fn default() -> Self {
        let events = crate::protocol::STREAMS
            .iter()
            .map(|stream| (*stream, Source::new(stream)))
            .collect();
        Self {
            snapshot: Arc::new(RwLock::new(BarSnapshot::default())),
            events: Arc::new(events),
            failed_monitors: Arc::default(),
            work_area_demand: tokio::sync::watch::channel(0).0,
            hyprland_connected: Arc::new(AtomicBool::new(false)),
            work_area_changed: Arc::new(tokio::sync::Notify::new()),
            compositor_changed: Arc::new(tokio::sync::Notify::new()),
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
    pub(crate) fn stream_semantics(stream: &str) -> &'static str {
        if events::ordered(stream) {
            "ordered-state"
        } else {
            "replaceable-state"
        }
    }

    /// Read a projection under the snapshot lock without cloning unrelated domains.
    pub(crate) async fn read<T>(&self, project: impl FnOnce(&BarSnapshot) -> T) -> T {
        let snapshot = self.snapshot.read().await;
        if self.failed_monitors.lock().unwrap().is_empty() {
            project(&snapshot)
        } else {
            project(&self.healthy_snapshot(&snapshot))
        }
    }

    pub(crate) async fn snapshot(&self) -> BarSnapshot {
        let started = Instant::now();
        let snapshot = self.healthy_snapshot(&*self.snapshot.read().await);
        tracing::debug!(
            latency_us = started.elapsed().as_micros() as u64,
            "bar snapshot read"
        );
        snapshot
    }

    /// Receiver registration and initial state share the commit lock. No update
    /// can fall between the initial projection and domain source registration.
    pub(crate) async fn snapshot_and_subscribe(&self) -> (BarSnapshot, DomainReceivers) {
        self.snapshot_and_subscribe_domains(crate::protocol::STREAMS)
            .await
    }

    pub(crate) async fn snapshot_and_subscribe_domains(
        &self,
        streams: &[&str],
    ) -> (BarSnapshot, DomainReceivers) {
        let snapshot = self.snapshot.read().await;
        let events = self.subscribe_domains(streams);
        (self.healthy_snapshot(&snapshot), events)
    }

    pub(crate) fn subscribe(&self) -> DomainReceivers {
        self.subscribe_domains(crate::protocol::STREAMS)
    }

    pub(crate) fn subscribe_domains(&self, streams: &[&str]) -> DomainReceivers {
        DomainReceivers(
            streams
                .iter()
                .map(|stream| self.events[stream].subscribe())
                .collect(),
        )
    }

    fn unhealthy(&self, stream: &str, data: &mut Value) {
        if let Some(reason) = self.failed_monitors.lock().unwrap().get(stream) {
            data["available"] = Value::Bool(false);
            data["error"] = Value::String(reason.clone());
        }
    }

    pub(crate) fn fence_reply(&self, mut response: Value) -> Value {
        if self.failed_monitors.lock().unwrap().is_empty() {
            return response;
        }
        if let Some(data) = response.get_mut("data") {
            for stream in crate::protocol::STREAMS {
                let key = crate::protocol::read_wire::snapshot_key(stream);
                if let Some(value) = data.get_mut(key) {
                    self.unhealthy(stream, value);
                }
                if let Some(value) = data
                    .get_mut("snapshot")
                    .and_then(|snapshot| snapshot.get_mut(key))
                {
                    self.unhealthy(stream, value);
                }
            }
        }
        response
    }

    fn healthy_snapshot(&self, snapshot: &BarSnapshot) -> BarSnapshot {
        if self.failed_monitors.lock().unwrap().is_empty() {
            return snapshot.clone();
        }
        let mut value = to_value(snapshot).expect("snapshot serialization");
        for stream in crate::protocol::STREAMS {
            self.unhealthy(
                stream,
                &mut value[crate::protocol::read_wire::snapshot_key(stream)],
            );
        }
        serde_json::from_value(value).expect("availability overlay preserves snapshot types")
    }

    /// Sticky until daemon restart. Never blindly restart a policy engine after
    /// a panic: effects/inhibitors may have unknown outcomes. Other domains live.
    pub(crate) async fn monitor_failed(&self, name: &str, streams: &[&'static str], reason: &str) {
        let snapshot = self.snapshot.write().await;
        {
            let mut failures = self.failed_monitors.lock().unwrap();
            for stream in streams {
                failures.insert(
                    stream,
                    format!("{name} monitor {reason}; restart daemon to recover"),
                );
            }
        }
        for stream in streams {
            self.publish(
                stream,
                &crate::protocol::read_wire::initial_stream_data(stream, &snapshot),
            );
        }
        tracing::error!(
            monitor = name,
            available = false,
            reason,
            "bar monitor stopped"
        );
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

    /// The Updates catalog changed without changing the worker's current state.
    pub(crate) async fn invalidate_update_history(&self) {
        let snapshot = self.snapshot.read().await;
        self.publish(crate::protocol::stream::UPDATES, &snapshot.updates);
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

    pub(crate) fn hyprland_connected(&self) -> bool {
        self.hyprland_connected.load(Ordering::Relaxed)
    }

    pub(crate) fn set_hyprland_connected(&self, connected: bool) {
        self.hyprland_connected.store(connected, Ordering::Relaxed);
        // Every connection transition invalidates caches, even if several
        // transitions coalesce and the final connectivity value is unchanged.
        self.work_area_changed.notify_one();
        self.compositor_changed.notify_one();
    }

    pub(crate) fn work_area_interest(&self) -> WorkAreaInterest {
        self.work_area_demand.send_modify(|count| *count += 1);
        WorkAreaInterest(self.work_area_demand.clone())
    }
    pub(crate) fn work_area_demand(&self) -> tokio::sync::watch::Receiver<usize> {
        self.work_area_demand.subscribe()
    }
    pub(crate) async fn update_work_area(&self, mut value: WorkAreaState) {
        let mut snapshot = self.snapshot.write().await;
        value.revision = snapshot.workarea.revision;
        if value == snapshot.workarea {
            return;
        }
        value.revision = value.revision.saturating_add(1);
        snapshot.workarea = value;
        self.publish(crate::protocol::stream::WORKAREA, &snapshot.workarea);
    }

    pub(crate) async fn update_compositor(&self, mut value: CompositorState) {
        let mut snapshot = self.snapshot.write().await;
        value.revision = snapshot.compositor.revision;
        if value == snapshot.compositor {
            return;
        }
        value.revision = value.revision.saturating_add(1);
        snapshot.compositor = value;
        self.publish(crate::protocol::stream::COMPOSITOR, &snapshot.compositor);
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
        let mut data = to_value(value).unwrap_or(Value::Null);
        self.unhealthy(stream, &mut data);
        self.events[stream].send(DomainEvent {
            stream: stream.into(),
            data,
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::StateStore;

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
