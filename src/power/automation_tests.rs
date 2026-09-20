//! In-process Power Profiles substitute; never touches the system bus/hardware.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::net::UnixStream;
use zbus::{Connection, connection::Builder};
use zvariant::OwnedValue;

use super::automation::PowerEnvelope;
use super::*;
use crate::model::BatteryProfileAction;

#[derive(Default)]
struct FakeState {
    selected: String,
    holds: Vec<(u32, String, String)>,
    next_cookie: u32,
    acquisitions: usize,
    selections: usize,
    fail_selection: bool,
}

impl FakeState {
    fn profile(&self) -> String {
        if self
            .holds
            .iter()
            .any(|(_, profile, _)| profile == "power-saver")
        {
            "power-saver".into()
        } else if !self.holds.is_empty() {
            "performance".into()
        } else {
            self.selected.clone()
        }
    }
}

struct FakeProfiles(Arc<Mutex<FakeState>>);

fn string(value: &str) -> OwnedValue {
    OwnedValue::from(zvariant::Str::from(value.to_string()))
}

#[zbus::interface(name = "org.freedesktop.UPower.PowerProfiles")]
impl FakeProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.0.lock().unwrap().profile()
    }

    #[zbus(property)]
    async fn set_active_profile(&self, profile: &str) -> zbus::Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.fail_selection {
            return Err(zbus::Error::Failure("test selection failure".into()));
        }
        state.selected = profile.into();
        state.holds.clear();
        state.selections += 1;
        Ok(())
    }

    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        ["power-saver", "balanced", "performance"]
            .iter()
            .map(|profile| HashMap::from([("Profile".into(), string(profile))]))
            .collect()
    }

    #[zbus(property)]
    fn active_profile_holds(&self) -> Vec<HashMap<String, OwnedValue>> {
        self.0
            .lock()
            .unwrap()
            .holds
            .iter()
            .map(|(_, profile, owner)| {
                HashMap::from([
                    ("Profile".into(), string(profile)),
                    ("ApplicationId".into(), string(owner)),
                ])
            })
            .collect()
    }

    #[zbus(property)]
    fn actions_info(&self) -> Vec<HashMap<String, OwnedValue>> {
        vec![HashMap::from([
            ("Name".into(), string("trickle_charge")),
            ("Enabled".into(), OwnedValue::from(false)),
            ("Description".into(), string("Charging behaviour")),
        ])]
    }

    fn hold_profile(
        &self,
        profile: &str,
        _reason: &str,
        application_id: &str,
    ) -> zbus::fdo::Result<u32> {
        if !matches!(profile, "power-saver" | "performance") {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Balanced cannot be held".into(),
            ));
        }
        let mut state = self.0.lock().unwrap();
        state.next_cookie += 1;
        let cookie = state.next_cookie;
        state
            .holds
            .push((cookie, profile.into(), application_id.into()));
        state.acquisitions += 1;
        Ok(cookie)
    }

    fn release_profile(&self, cookie: u32) {
        self.0
            .lock()
            .unwrap()
            .holds
            .retain(|(id, _, _)| *id != cookie);
    }
}

async fn fake_profiles() -> (Connection, Connection, Arc<Mutex<FakeState>>) {
    let state = Arc::new(Mutex::new(FakeState {
        selected: "performance".into(),
        ..Default::default()
    }));
    let (server, client) = UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server)
        .server(zbus::Guid::generate())
        .unwrap()
        .p2p()
        .serve_at(PATH, FakeProfiles(Arc::clone(&state)))
        .unwrap()
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::try_join!(server, client).unwrap();
    (server, client, state)
}

fn battery(percent: u8) -> BatteryState {
    BatteryState {
        available: true,
        percentage: percent,
        ..Default::default()
    }
}

#[tokio::test]
async fn holds_are_cooperative_deduplicated_and_manual_override_can_resume() {
    let (_server, client, state) = fake_profiles().await;
    let directory = tempfile::tempdir().unwrap();
    let mut envelope = PowerEnvelope::for_test(directory.path().join("state.json"));
    envelope.attach(client.clone()).await.unwrap();
    state
        .lock()
        .unwrap()
        .holds
        .push((99, "performance".into(), "other-app".into()));
    for percent in [25, 24, 12, 10] {
        envelope.reconcile(&battery(percent)).await.unwrap();
        assert_eq!(state.lock().unwrap().profile(), "power-saver");
    }
    assert_eq!(state.lock().unwrap().acquisitions, 1);
    assert_eq!(state.lock().unwrap().holds.len(), 2);
    let mut plugged = battery(10);
    plugged.plugged = true;
    envelope.reconcile(&plugged).await.unwrap();
    assert_eq!(
        state.lock().unwrap().holds.len(),
        1,
        "release only our hold"
    );
    assert_eq!(state.lock().unwrap().selected, "performance");

    envelope.reconcile(&battery(10)).await.unwrap();
    envelope
        .select_manual("balanced", &battery(10), &client)
        .await
        .unwrap();
    envelope.reconcile(&battery(9)).await.unwrap();
    assert_eq!(envelope.status.status, "paused");
    assert_eq!(state.lock().unwrap().profile(), "balanced");
    envelope.resume(&battery(9)).await.unwrap();
    assert_eq!(state.lock().unwrap().profile(), "power-saver");
    envelope.reconcile(&plugged).await.unwrap();
    assert_eq!(state.lock().unwrap().profile(), "balanced");
    let raw = read_raw_state(&client).await.unwrap();
    assert_eq!(raw.actions[0].name, "trickle_charge");
    assert!(!raw.actions[0].enabled);
}

#[tokio::test]
async fn balanced_defers_to_other_holds_and_restores_across_restart() {
    let (_server, client, state) = fake_profiles().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let mut envelope = PowerEnvelope::for_test(path.clone());
    envelope.attach(client.clone()).await.unwrap();
    let mut low = battery(25);
    low.policy.warning_profile = BatteryProfileAction::Balanced;
    state
        .lock()
        .unwrap()
        .holds
        .push((99, "performance".into(), "other-app".into()));
    envelope.reconcile(&low).await.unwrap();
    assert_eq!(envelope.status.status, "blocked");
    assert_eq!(state.lock().unwrap().selections, 0);
    state.lock().unwrap().holds.clear();
    envelope.reconcile(&low).await.unwrap();
    assert_eq!(state.lock().unwrap().profile(), "balanced");
    envelope.reconcile(&low).await.unwrap();
    assert_eq!(state.lock().unwrap().selections, 1);
    drop(envelope);

    let mut restarted = PowerEnvelope::for_test(path);
    restarted.attach(client.clone()).await.unwrap();
    restarted.reconcile(&BatteryState::default()).await.unwrap();
    assert_eq!(
        state.lock().unwrap().profile(),
        "balanced",
        "startup without telemetry must not restore prematurely"
    );
    restarted.reconcile(&low).await.unwrap();
    let mut critical = low.clone();
    critical.percentage = 12;
    restarted.reconcile(&critical).await.unwrap();
    assert_eq!(state.lock().unwrap().profile(), "power-saver");
    assert_eq!(
        state.lock().unwrap().selected,
        "performance",
        "restore base before taking a hold"
    );
    restarted.reconcile(&low).await.unwrap();
    assert_eq!(state.lock().unwrap().profile(), "balanced");
    low.plugged = true;
    restarted.reconcile(&low).await.unwrap();
    assert_eq!(state.lock().unwrap().profile(), "performance");
}

#[tokio::test]
async fn failed_manual_selection_restores_automation_and_external_selection_pauses_it() {
    let (_server, client, state) = fake_profiles().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let mut envelope = PowerEnvelope::for_test(path.clone());
    envelope.attach(client.clone()).await.unwrap();
    envelope.reconcile(&battery(20)).await.unwrap();
    state.lock().unwrap().fail_selection = true;
    assert!(
        envelope
            .select_manual("balanced", &battery(20), &client)
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().profile(), "power-saver");
    assert_eq!(envelope.status.status, "active");
    {
        let mut state = state.lock().unwrap();
        state.fail_selection = false;
        state.holds.clear();
        state.selected = "balanced".into();
    }
    envelope.reconcile(&battery(20)).await.unwrap();
    assert_eq!(envelope.status.status, "paused");
    drop(envelope);
    let mut restarted = PowerEnvelope::for_test(path);
    restarted.attach(client).await.unwrap();
    restarted.reconcile(&battery(27)).await.unwrap();
    assert_eq!(
        restarted.status.status, "paused",
        "restart preserves the recovery margin"
    );
    for percent in [10, 15, 26, 28] {
        restarted.reconcile(&battery(percent)).await.unwrap();
        assert_eq!(restarted.status.status, "paused");
        assert_eq!(state.lock().unwrap().profile(), "balanced");
    }
    restarted.reconcile(&battery(29)).await.unwrap();
    assert_eq!(restarted.status.status, "waiting");
    restarted.reconcile(&battery(25)).await.unwrap();
    assert_eq!(restarted.status.status, "active");
}
