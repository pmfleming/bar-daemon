use super::*;
use std::cell::{Cell, RefCell};

struct Fake {
    outputs: RefCell<Vec<Output>>,
    calls: RefCell<Vec<String>>,
    fail: Cell<bool>,
}
impl Backend for Fake {
    async fn eligible(&self) -> Result<()> {
        Ok(())
    }
    async fn outputs(&self) -> Result<Vec<Output>> {
        Ok(self.outputs.borrow().clone())
    }
    async fn apply(&self, _: &Output, _: bool) -> Result<()> {
        unreachable!()
    }
    async fn configure(&self, setting: &Setting) -> Result<()> {
        self.calls.borrow_mut().push(setting.command()?);
        if self.fail.get() {
            bail!("compositor failure")
        }
        let mut outputs = self.outputs.borrow_mut();
        let output = outputs.iter_mut().find(|o| o.name == setting.name).unwrap();
        output.disabled = !setting.enabled;
        let (width, height, refresh) = mode(&setting.mode).unwrap();
        output.width = width;
        output.height = height;
        output.refresh_rate = refresh;
        output.x = setting.x;
        output.y = setting.y;
        output.scale = setting.scale;
        output.transform = setting.transform;
        Ok(())
    }
}
fn fake() -> Fake {
    Fake {
        outputs: RefCell::new(vec![Output {
            name: "eDP-1".into(),
            id: 0,
            width: 1920,
            height: 1200,
            scale: 1.25,
            refresh_rate: 60.0,
            available_modes: vec!["1920x1200@60.00Hz".into()],
            ..Default::default()
        }]),
        calls: RefCell::new(vec![]),
        fail: Cell::new(false),
    }
}

#[tokio::test]
async fn policy_changes_are_blocked_by_durable_trials_including_after_restart() {
    let backend = fake();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let store = StateStore::default();
    let mut runtime = Runtime::default();
    ensure_policy_change_allowed_at(&path).await.unwrap();
    let proposed = Layout::observed(&backend.outputs.borrow());
    let doc = preview_at(&backend, proposed, &store, &path, &mut runtime)
        .await
        .unwrap();
    assert!(ensure_policy_change_allowed_at(&path).await.is_err());
    // The durable document, not runtime state or a connected frontend, gates policy.
    runtime = Runtime::default();
    assert!(ensure_policy_change_allowed_at(&path).await.is_err());
    finish_at(
        &backend,
        &doc.trial.unwrap().id,
        false,
        &store,
        &path,
        &mut runtime,
    )
    .await
    .unwrap();
    ensure_policy_change_allowed_at(&path).await.unwrap();
}

#[tokio::test]
async fn replacement_with_same_connector_cannot_be_confirmed_and_rolls_back_early() {
    let backend = fake();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let store = StateStore::default();
    let mut runtime = Runtime::default();
    let mut proposed = Layout::observed(&backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    let doc = preview_at(&backend, proposed, &store, &path, &mut runtime)
        .await
        .unwrap();
    let trial = doc.trial.unwrap();
    backend.outputs.borrow_mut()[0].id += 1;
    let error = finish_at(&backend, &trial.id, true, &store, &path, &mut runtime)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("topology changed"));
    let (doc, paused) = tick_at(
        &backend,
        &store,
        &path,
        &mut runtime,
        trial.expires_at - 19,
        Instant::now(),
    )
    .await
    .unwrap();
    assert!(!paused);
    assert!(doc.trial.is_none());
    assert_eq!(backend.outputs.borrow()[0].scale, 1.25);
}

#[tokio::test]
async fn monotonic_timeout_and_failed_rollback_keep_recovery_intent() {
    let backend = fake();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let store = StateStore::default();
    let mut runtime = Runtime::default();
    let mut proposed = Layout::observed(&backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    preview_at(&backend, proposed, &store, &path, &mut runtime)
        .await
        .unwrap();
    let expired = Instant::now() + Duration::from_secs(21);
    backend.fail.set(true);
    assert!(
        tick_at(&backend, &store, &path, &mut runtime, 0, expired)
            .await
            .is_err()
    );
    assert!(ensure_policy_change_allowed_at(&path).await.is_err());
    backend.fail.set(false);
    let (doc, _) = tick_at(&backend, &store, &path, &mut runtime, 0, expired)
        .await
        .unwrap();
    assert!(doc.trial.is_none());
    assert_eq!(backend.outputs.borrow()[0].scale, 1.25);
}

#[tokio::test]
async fn replacements_are_enabled_before_disabling_and_failure_keeps_the_old_output() {
    for fail in [false, true] {
        let backend = fake();
        let mut old = backend.outputs.borrow()[0].clone();
        old.name = "DP-1".into();
        let mut replacement = old.clone();
        replacement.id = 1;
        replacement.name = "DP-2".into();
        replacement.disabled = true;
        *backend.outputs.borrow_mut() = vec![old, replacement];
        let mut proposed = Layout::observed(&backend.outputs.borrow());
        proposed.outputs[0].enabled = false;
        proposed.outputs[1].enabled = true;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let store = StateStore::default();
        backend.fail.set(fail);
        let result = preview_at(&backend, proposed, &store, &path, &mut Runtime::default()).await;
        assert_eq!(result.is_err(), fail);
        assert!(backend.calls.borrow()[0].contains("DP-2"));
        if fail {
            assert_eq!(backend.calls.borrow().len(), 1);
            assert!(!backend.outputs.borrow()[0].disabled);
        } else {
            assert!(backend.outputs.borrow()[0].disabled);
            assert!(!backend.outputs.borrow()[1].disabled);
        }
    }
}

#[test]
fn validates_names_modes_bounds_topology_and_fallback() {
    let backend = fake();
    let outputs = backend.outputs.borrow();
    let mut layout = Layout::observed(&outputs);
    assert!(layout.validate(&outputs, true).is_ok());
    layout.outputs[0].name = "eDP-1\"});os.execute('evil')".into();
    assert!(layout.validate(&outputs, true).is_err());
    layout.outputs[0].name = "eDP-1".into();
    for mode in [
        "1920x1200@60;evil",
        "1920x1200@NaN",
        "999999x1200@60",
        "1920x1200@75",
    ] {
        layout.outputs[0].mode = mode.into();
        assert!(layout.validate(&outputs, true).is_err());
    }
    layout.outputs[0].mode = "1920x1200@60".into();
    layout.outputs[0].enabled = false;
    assert!(layout.validate(&outputs, true).is_err());
    layout.outputs[0].enabled = true;
    layout.outputs[0].x = i32::MIN;
    assert!(layout.validate(&outputs, true).is_err());
    layout.outputs[0].x = 0;
    layout.outputs[0].scale = f64::NAN;
    assert!(layout.validate(&outputs, true).is_err());
}

#[tokio::test]
async fn expired_restart_and_resume_previews_roll_back_durably() {
    for recovery in ["expiry", "restart", "resume"] {
        let backend = fake();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let store = StateStore::default();
        let mut runtime = Runtime::default();
        let mut proposed = Layout::observed(&backend.outputs.borrow());
        proposed.outputs[0].scale = 1.5;
        let doc = preview_at(&backend, proposed, &store, &path, &mut runtime)
            .await
            .unwrap();
        assert_eq!(backend.outputs.borrow()[0].scale, 1.5);
        let trial = doc.trial.unwrap();
        let clock = if recovery == "expiry" {
            trial.expires_at
        } else {
            trial.expires_at - 1
        };
        if recovery == "restart" {
            runtime = Runtime::default();
        }
        if recovery == "resume" {
            store.record_resume().await;
        }
        let (doc, paused) = tick_at(&backend, &store, &path, &mut runtime, clock, Instant::now())
            .await
            .unwrap();
        assert!(!paused);
        assert!(doc.trial.is_none());
        assert_eq!(backend.outputs.borrow()[0].scale, 1.25);
        assert!(
            load_json_or_default::<Document>(&path, "test")
                .await
                .unwrap()
                .trial
                .is_none()
        );
    }
}

#[tokio::test]
async fn confirmation_is_token_bound_verified_and_persistent() {
    let backend = fake();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let store = StateStore::default();
    let mut runtime = Runtime::default();
    let mut proposed = Layout::observed(&backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    let doc = preview_at(&backend, proposed.clone(), &store, &path, &mut runtime)
        .await
        .unwrap();
    let id = doc.trial.unwrap().id;
    assert!(
        finish_at(&backend, "stale", true, &store, &path, &mut runtime)
            .await
            .is_err()
    );
    backend.outputs.borrow_mut()[0].scale = 1.25;
    assert!(
        finish_at(&backend, &id, true, &store, &path, &mut runtime)
            .await
            .is_err()
    );
    backend.outputs.borrow_mut()[0].scale = 1.5;
    let doc = finish_at(&backend, &id, true, &store, &path, &mut runtime)
        .await
        .unwrap();
    assert_eq!(doc.saved, proposed);
    assert!(doc.trial.is_none());
    assert_eq!(
        load_json_or_default::<Document>(&path, "test")
            .await
            .unwrap(),
        doc
    );
    assert!(
        finish_at(&backend, &id, true, &store, &path, &mut runtime)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_apply_keeps_rollback_intent_and_sleep_defers_recovery() {
    let backend = fake();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let store = StateStore::default();
    let mut runtime = Runtime::default();
    let mut proposed = Layout::observed(&backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    backend.fail.set(true);
    assert!(
        preview_at(&backend, proposed, &store, &path, &mut runtime)
            .await
            .is_err()
    );
    assert!(
        load_json_or_default::<Document>(&path, "test")
            .await
            .unwrap()
            .trial
            .is_some()
    );
    backend.calls.borrow_mut().clear();
    store.record_sleep_preparation(true).await;
    assert!(
        tick_at(
            &backend,
            &store,
            &path,
            &mut runtime,
            u64::MAX,
            Instant::now()
        )
        .await
        .unwrap()
        .1
    );
    assert!(backend.calls.borrow().is_empty());
    store.record_resume().await;
    backend.fail.set(false);
    assert!(
        tick_at(
            &backend,
            &store,
            &path,
            &mut runtime,
            u64::MAX,
            Instant::now()
        )
        .await
        .unwrap()
        .0
        .trial
        .is_none()
    );
}

#[tokio::test]
async fn saved_layout_waits_for_topology_and_does_not_reapply_working_modes() {
    let backend = fake();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let store = StateStore::default();
    let mut runtime = Runtime::default();
    let mut saved = Layout::observed(&backend.outputs.borrow());
    saved.outputs[0].scale = 1.5;
    save_json_atomic(&path, &Document { saved, trial: None })
        .await
        .unwrap();
    let start = Instant::now();
    assert!(
        tick_at(&backend, &store, &path, &mut runtime, 0, start)
            .await
            .unwrap()
            .1
    );
    assert!(backend.calls.borrow().is_empty());
    tick_at(
        &backend,
        &store,
        &path,
        &mut runtime,
        5,
        start + Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(backend.calls.borrow().len(), 1);
    tick_at(
        &backend,
        &store,
        &path,
        &mut runtime,
        10,
        start + Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(backend.calls.borrow().len(), 1);
}
