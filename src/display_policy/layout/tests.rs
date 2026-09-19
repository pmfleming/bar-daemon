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
