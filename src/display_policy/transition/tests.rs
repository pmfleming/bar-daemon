use super::*;

#[test]
fn acknowledgement_waits_for_observation_and_timeout_has_explicit_recovery() {
    let mut transition = Transition::default();
    let mut state = DisplayPolicyState {
        docking_state: DockingState::Applying,
        ..Default::default()
    };
    let start = Instant::now();
    for seconds in [0, 5, 19] {
        transition.update(
            &mut state,
            Ok("pending"),
            start + Duration::from_secs(seconds),
        );
        assert_eq!(state.docking_state, DockingState::Applying);
    }
    transition.update(&mut state, Ok("pending"), start + APPLY_TIMEOUT);
    assert_eq!(state.docking_state, DockingState::Failed);
    assert_eq!(state.status, "error");
    let error = state.error.clone();
    transition.update(&mut state, Ok("layout"), start + APPLY_TIMEOUT);
    assert_eq!(state.docking_state, DockingState::Failed);
    assert_eq!(
        state.error, error,
        "a paused failed policy is not a successful retry"
    );
    // Explicitly re-saving the same preference starts a new observation window.
    state.docking_state = DockingState::Applying;
    transition.update(&mut state, Ok("pending"), start + Duration::from_secs(30));
    assert_eq!(state.docking_state, DockingState::Applying);
    transition.update(
        &mut state,
        Ok("all-displays"),
        start + Duration::from_secs(31),
    );
    assert_eq!(state.docking_state, DockingState::Settled);
    assert!(state.error.is_none());
}

#[test]
fn settling_and_layout_recovery_are_not_completed_docking() {
    let mut transition = Transition::default();
    let mut state = DisplayPolicyState::default();
    let start = Instant::now();
    for status in ["layout-preview", "settling", "pending"] {
        transition.update(&mut state, Ok(status), start);
        assert_eq!(state.docking_state, DockingState::Applying);
    }
    transition.update(&mut state, Ok("external"), start + Duration::from_secs(1));
    assert_eq!(
        state.docking_state,
        DockingState::Settled,
        "real readback need not wait for the deadline"
    );
}

#[test]
fn failure_is_sticky_but_confirmed_manual_enablement_supersedes_it() {
    let mut transition = Transition::default();
    let mut state = DisplayPolicyState::default();
    let now = Instant::now();
    transition.update(
        &mut state,
        Err(anyhow::anyhow!("compositor rejected the rule")),
        now,
    );
    assert_eq!(state.docking_state, DockingState::Failed);
    // A preview/revert alone must not restart the failed policy.
    transition.update(&mut state, Ok("layout-preview"), now);
    assert_eq!(state.docking_state, DockingState::Failed);
    transition.update(&mut state, Ok("internal"), now);
    assert_eq!(
        state.docking_state,
        DockingState::Failed,
        "emergency fallback does not claim policy success"
    );
    state.layout.manual_enablement = true;
    transition.update(&mut state, Ok("layout"), now);
    assert_eq!(state.docking_state, DockingState::Settled);
    assert!(state.error.is_none());
}

#[test]
fn sleep_does_not_acknowledge_a_pending_change_or_consume_its_resume_budget() {
    let mut transition = Transition::default();
    let mut state = DisplayPolicyState::default();
    let start = Instant::now();
    transition.update(&mut state, Ok("pending"), start);
    transition.update(&mut state, Ok("sleeping"), start + Duration::from_secs(5));
    assert_eq!(state.docking_state, DockingState::Applying);
    transition.update(&mut state, Ok("settling"), start + Duration::from_secs(100));
    assert_eq!(state.docking_state, DockingState::Applying);
    transition.update(&mut state, Ok("external"), start + Duration::from_secs(106));
    assert_eq!(state.docking_state, DockingState::Settled);
}
