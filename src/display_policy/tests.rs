use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

use super::{Backend, DisplayPolicy, Output, Planner, reconcile};
use crate::{
    paths::{load_json_or_default, save_json_atomic},
    state::StateStore,
};
use anyhow::{Result, bail};
use std::time::{Duration, Instant};

fn internal(disabled: bool) -> Output {
    Output {
        id: 0,
        name: "eDP-1".into(),
        width: 1920,
        height: 1200,
        disabled,
        scale: 1.25,
        refresh_rate: 60.0,
        ..Default::default()
    }
}
fn external() -> Output {
    Output {
        id: 1,
        name: "HDMI-A-1".into(),
        width: 3440,
        height: 1440,
        disabled: false,
        scale: 1.25,
        refresh_rate: 75.0,
        ..Default::default()
    }
}

#[test]
fn laptop_travel_dock_wake_and_unplug_preserve_a_fallback() {
    let mut planner = Planner::default();
    let start = Instant::now();
    assert_eq!(
        planner.plan(true, &[internal(false)], start).status,
        "internal"
    );
    let docked = vec![internal(false), external()];
    assert_eq!(planner.plan(true, &docked, start).status, "settling");
    assert!(
        !planner
            .plan(true, &docked, start + Duration::from_secs(4))
            .disable_internal
    );
    let ready = planner.plan(true, &docked, start + Duration::from_secs(5));
    assert!(ready.disable_internal);
    assert_eq!(ready.targets.len(), 1);
    assert_eq!(ready.targets[0].name, "eDP-1");
    // A daemon-confirmed resume invalidates pre-sleep stability even though
    // CLOCK_MONOTONIC did not advance while the machine slept.
    planner.reset();
    let resumed = [internal(true), external()];
    let wake = planner.plan(true, &resumed, start + Duration::from_secs(5));
    assert_eq!(wake.status, "settling");
    assert!(!wake.disable_internal);
    assert_eq!(wake.targets.len(), 1);
    let unplugged = [internal(true)];
    let unplug = planner.plan(true, &unplugged, start + Duration::from_secs(6));
    assert_eq!(unplug.status, "internal");
    assert_eq!(unplug.targets.len(), 1);
    // A compositor accepting a command is not proof that it enabled the panel.
    assert_eq!(
        planner
            .plan(true, &[internal(true)], start + Duration::from_secs(8))
            .targets
            .len(),
        1
    );
}

#[test]
fn output_flaps_mode_changes_and_observation_gaps_restart_stability() {
    let start = Instant::now();
    let mut planner = Planner::default();
    let outputs = vec![internal(false), external()];
    planner.plan(true, &outputs, start);
    planner.plan(true, &outputs, start + Duration::from_secs(4));
    let mut replacement = external();
    replacement.id = 2;
    let changed = vec![internal(false), replacement];
    assert_eq!(
        planner
            .plan(true, &changed, start + Duration::from_secs(5))
            .status,
        "settling"
    );
    assert_eq!(
        planner
            .plan(true, &changed, start + Duration::from_secs(10))
            .status,
        "external"
    );
    assert_eq!(
        planner
            .plan(true, &changed, start + Duration::from_secs(30))
            .status,
        "settling"
    );
    let mut missing = external();
    missing.disabled = true;
    assert_eq!(
        planner
            .plan(
                true,
                &[internal(true), missing],
                start + Duration::from_secs(31)
            )
            .status,
        "internal"
    );
}

#[test]
fn dpms_off_is_not_output_loss_and_preference_off_restores_internal() {
    let external: Output = serde_json::from_value(serde_json::json!({
        "id": 1, "name": "DP-1", "width": 3440, "height": 1440,
        "scale": 1.25, "refreshRate": 75, "disabled": false, "dpmsStatus": false
    }))
    .unwrap();
    let start = Instant::now();
    let mut planner = Planner::default();
    let outputs = vec![internal(true), external];
    planner.plan(true, &outputs, start);
    assert!(
        planner
            .plan(true, &outputs, start + Duration::from_secs(5))
            .targets
            .is_empty()
    );
    let off = planner.plan(false, &outputs, start + Duration::from_secs(6));
    assert!(!off.disable_internal);
    assert_eq!(off.targets.len(), 1);
    assert!(
        off.targets[0]
            .command(false)
            .unwrap()
            .contains("scale = 1.25")
    );
}

#[test]
fn docking_preserves_mirror_sources_and_restores_only_independent_fallbacks() {
    let start = Instant::now();
    let mut copy = external();
    copy.mirror_of = "0".into();
    let mut third = external();
    third.id = 2;
    third.name = "DP-2".into();
    for outputs in [
        vec![internal(false), copy.clone()],
        vec![internal(false), copy, third],
    ] {
        let mut planner = Planner::default();
        planner.plan(true, &outputs, start);
        assert!(
            planner
                .plan(true, &outputs, start + Duration::from_secs(5))
                .targets
                .is_empty()
        );
    }
    let mut laptop_copy = internal(false);
    laptop_copy.mirror_of = "1".into();
    let mut planner = Planner::default();
    assert!(
        planner
            .plan(false, &[laptop_copy.clone(), external()], start)
            .targets
            .is_empty()
    );
    assert_eq!(
        planner
            .plan(false, &[laptop_copy.clone()], start)
            .targets
            .len(),
        1
    );
    assert!(
        laptop_copy
            .command(false)
            .unwrap()
            .contains("mirror = \"\"")
    );
}

#[derive(Default)]
struct FakeBackend {
    snapshots: RefCell<VecDeque<Vec<Output>>>,
    calls: RefCell<Vec<(String, bool)>>,
    conflict: Cell<bool>,
    fail: Cell<bool>,
    reads: Cell<usize>,
    resume_on_second_read: Option<StateStore>,
}
impl Backend for FakeBackend {
    async fn eligible(&self) -> Result<()> {
        if self.conflict.get() {
            bail!("old monitor service is active");
        }
        Ok(())
    }
    async fn outputs(&self) -> Result<Vec<Output>> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == 2 {
            if let Some(store) = &self.resume_on_second_read {
                store.record_resume().await;
            }
        }
        let mut snapshots = self.snapshots.borrow_mut();
        Ok(if snapshots.len() > 1 {
            snapshots.pop_front().unwrap()
        } else {
            snapshots.front().unwrap().clone()
        })
    }
    async fn apply(&self, output: &Output, disable: bool) -> Result<()> {
        self.calls.borrow_mut().push((output.name.clone(), disable));
        if self.fail.get() {
            bail!("compositor rejected the rule");
        }
        Ok(())
    }
}

#[tokio::test]
async fn final_hotplug_and_sleep_checks_prevent_disabling_the_only_display() {
    for resume in [false, true] {
        let store = StateStore::default();
        let start = Instant::now();
        let docked = vec![internal(false), external()];
        let mut planner = Planner::default();
        planner.plan(true, &docked, start);
        let backend = FakeBackend {
            snapshots: RefCell::new(VecDeque::from([
                docked.clone(),
                if resume {
                    docked
                } else {
                    vec![internal(false)]
                },
            ])),
            resume_on_second_read: resume.then(|| store.clone()),
            ..Default::default()
        };
        assert!(
            reconcile(
                &backend,
                &mut planner,
                &DisplayPolicy::default(),
                false,
                &store,
                start + Duration::from_secs(5)
            )
            .await
            .is_err()
        );
        assert!(backend.calls.borrow().is_empty());
    }
}

#[tokio::test]
async fn a_new_mirror_dependency_blocks_a_previously_planned_source_disable() {
    let store = StateStore::default();
    let start = Instant::now();
    let docked = vec![internal(false), external()];
    let mut current = docked.clone();
    let mut mirror = external();
    mirror.name = "DP-2".into();
    mirror.id = 2;
    mirror.mirror_of = "0".into();
    current.push(mirror);
    let backend = FakeBackend {
        snapshots: RefCell::new(VecDeque::from([docked.clone(), current])),
        ..Default::default()
    };
    let mut planner = Planner::default();
    planner.plan(true, &docked, start);
    assert!(
        reconcile(
            &backend,
            &mut planner,
            &DisplayPolicy::default(),
            false,
            &store,
            start + Duration::from_secs(5)
        )
        .await
        .is_err()
    );
    assert!(backend.calls.borrow().is_empty());
}

#[tokio::test]
async fn conflicts_and_sleep_preparation_do_not_mutate_displays_and_failures_retry() {
    let store = StateStore::default();
    let backend = FakeBackend {
        snapshots: RefCell::new(VecDeque::from([vec![internal(true)]])),
        ..Default::default()
    };
    let mut planner = Planner::default();
    backend.conflict.set(true);
    assert!(
        reconcile(
            &backend,
            &mut planner,
            &DisplayPolicy::default(),
            false,
            &store,
            Instant::now()
        )
        .await
        .is_err()
    );
    assert!(backend.calls.borrow().is_empty());
    backend.conflict.set(false);
    store.record_sleep_preparation(true).await;
    assert_eq!(
        reconcile(
            &backend,
            &mut planner,
            &DisplayPolicy::default(),
            false,
            &store,
            Instant::now()
        )
        .await
        .unwrap(),
        "sleeping"
    );
    assert!(backend.calls.borrow().is_empty());
    store.record_resume().await;
    backend.fail.set(true);
    assert!(
        reconcile(
            &backend,
            &mut planner,
            &DisplayPolicy::default(),
            false,
            &store,
            Instant::now()
        )
        .await
        .is_err()
    );
    backend.fail.set(false);
    assert_eq!(
        reconcile(
            &backend,
            &mut planner,
            &DisplayPolicy::default(),
            false,
            &store,
            Instant::now()
        )
        .await
        .unwrap(),
        "internal"
    );
    assert_eq!(
        *backend.calls.borrow(),
        vec![("eDP-1".into(), false), ("eDP-1".into(), false)]
    );
}

#[tokio::test]
async fn manual_enablement_survives_policy_ticks_but_unplug_restores_fallback() {
    for prefer_external in [false, true] {
        for laptop_disabled in [false, true] {
            let backend = FakeBackend {
                snapshots: RefCell::new(VecDeque::from([vec![
                    internal(laptop_disabled),
                    external(),
                ]])),
                ..Default::default()
            };
            let store = StateStore::default();
            let mut planner = Planner::default();
            let policy = DisplayPolicy { prefer_external };
            let start = Instant::now();
            for seconds in [0, 5, 10] {
                assert_eq!(
                    reconcile(
                        &backend,
                        &mut planner,
                        &policy,
                        true,
                        &store,
                        start + Duration::from_secs(seconds)
                    )
                    .await
                    .unwrap(),
                    "layout"
                );
            }
            assert!(backend.calls.borrow().is_empty());
            *backend.snapshots.borrow_mut() = VecDeque::from([vec![internal(true)]]);
            reconcile(
                &backend,
                &mut planner,
                &policy,
                true,
                &store,
                start + Duration::from_secs(12),
            )
            .await
            .unwrap();
            assert_eq!(*backend.calls.borrow(), vec![("eDP-1".into(), false)]);
        }
    }
}

#[tokio::test]
async fn preferences_are_validated_and_persisted_without_arbitrary_commands() {
    for input in [
        r#"{}"#,
        r#"{"prefer_external":"yes"}"#,
        r#"{"prefer_external":true,"command":"exec"}"#,
    ] {
        assert!(serde_json::from_str::<DisplayPolicy>(input).is_err());
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("displays.json");
    let policy = DisplayPolicy {
        prefer_external: false,
    };
    save_json_atomic(&path, &policy).await.unwrap();
    assert_eq!(
        load_json_or_default::<DisplayPolicy>(&path, "test")
            .await
            .unwrap(),
        policy
    );
    let mut invalid = internal(true);
    invalid.name = "eDP-1\"}); os.execute('anything')".into();
    assert!(invalid.command(false).is_err());
    assert!(external().command(true).is_err());
}
