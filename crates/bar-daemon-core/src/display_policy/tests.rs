use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

use super::{Backend, DisplayPolicy, Output, Planner, reconcile};
use crate::state::StateStore;
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
    ignore_apply: Cell<bool>,
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
        if self.reads.get() == 2
            && let Some(store) = &self.resume_on_second_read
        {
            store.record_resume().await;
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
        if !self.ignore_apply.get() {
            for snapshot in self.snapshots.borrow_mut().iter_mut() {
                if let Some(current) = snapshot
                    .iter_mut()
                    .find(|o| o.name == output.name && o.id == output.id)
                {
                    current.disabled = disable;
                    if !disable {
                        current.mirror_of.clear();
                    }
                }
            }
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
        .unwrap()
        .status,
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
        .unwrap()
        .status,
        "internal"
    );
    assert_eq!(
        *backend.calls.borrow(),
        vec![("eDP-1".into(), false), ("eDP-1".into(), false)]
    );
}

#[tokio::test]
async fn reconciliation_requires_observed_enablement_in_both_directions() {
    for prefer_external in [false, true] {
        let start = Instant::now();
        let outputs = vec![internal(!prefer_external), external()];
        let backend = FakeBackend {
            snapshots: RefCell::new(VecDeque::from([outputs.clone()])),
            ignore_apply: Cell::new(true),
            ..Default::default()
        };
        let store = StateStore::default();
        let policy = DisplayPolicy { prefer_external };
        let mut planner = Planner::default();
        planner.plan(prefer_external, &outputs, start);
        let mut transition = super::Transition::default();
        let mut state = super::DisplayPolicyState::default();
        let observation = reconcile(
            &backend,
            &mut planner,
            &policy,
            false,
            &store,
            start + Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(
            observation.status, "pending",
            "ok is not hardware completion"
        );
        assert_eq!(observation.outputs.unwrap()[0].disabled, !prefer_external);
        transition.update(&mut state, Ok(observation.status), start);
        assert_eq!(state.docking_state, super::DockingState::Applying);
        // A later native snapshot supplies the requested state.
        backend.snapshots.borrow_mut().back_mut().unwrap()[0].disabled = prefer_external;
        let observed = reconcile(
            &backend,
            &mut planner,
            &policy,
            false,
            &store,
            start + Duration::from_secs(6),
        )
        .await
        .unwrap();
        assert_eq!(
            backend.calls.borrow().len(),
            1,
            "observed success needs no replay"
        );
        assert_eq!(observed.outputs.unwrap()[0].disabled, prefer_external);
        transition.update(
            &mut state,
            Ok(observed.status),
            start + Duration::from_secs(6),
        );
        assert_eq!(state.docking_state, super::DockingState::Settled);
    }
}

#[tokio::test]
async fn failed_docking_pauses_policy_but_unplug_still_restores_a_fallback() {
    let start = Instant::now();
    let backend = FakeBackend {
        snapshots: RefCell::new(VecDeque::from([vec![internal(true), external()]])),
        fail: Cell::new(true),
        ..Default::default()
    };
    let store = StateStore::default();
    let mut planner = Planner::default();
    let policy = DisplayPolicy {
        prefer_external: false,
    };
    let mut state = super::DisplayPolicyState::default();
    let mut transition = super::Transition::default();
    let failure = reconcile(&backend, &mut planner, &policy, false, &store, start).await;
    transition.update(&mut state, failure.map(|observed| observed.status), start);
    assert_eq!(state.docking_state, super::DockingState::Failed);
    backend.fail.set(false);
    for seconds in [2, 4] {
        let observed = reconcile(
            &backend,
            &mut planner,
            &policy,
            state.docking_state == super::DockingState::Failed,
            &store,
            start + Duration::from_secs(seconds),
        )
        .await
        .unwrap();
        transition.update(
            &mut state,
            Ok(observed.status),
            start + Duration::from_secs(seconds),
        );
        assert_eq!(state.docking_state, super::DockingState::Failed);
        assert_eq!(
            backend.calls.borrow().len(),
            1,
            "failed mutations are not silently retried"
        );
    }
    *backend.snapshots.borrow_mut() = VecDeque::from([vec![internal(true)]]);
    let observed = reconcile(
        &backend,
        &mut planner,
        &policy,
        true,
        &store,
        start + Duration::from_secs(6),
    )
    .await
    .unwrap();
    assert_eq!(observed.status, "all-displays");
    assert!(!observed.outputs.unwrap()[0].disabled);
    assert_eq!(
        backend.calls.borrow().len(),
        2,
        "failure must not suppress emergency enablement"
    );
}
