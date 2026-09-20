use super::{
    Backend, Document, Layout, Output, Runtime, Setting, ensure_policy_change_allowed_at,
    finish_at, mode, preview_at, tick_at,
};
use crate::{
    paths::{load_json_or_default, save_json_atomic},
    state::StateStore,
};
use anyhow::{Result, bail};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    time::{Duration, Instant},
};

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

// Own the persistent document and runtime together. Replacing runtime models a
// daemon restart without accidentally deleting the durable recovery document.
struct Fixture {
    backend: Fake,
    store: StateStore,
    runtime: Runtime,
    path: PathBuf,
    _directory: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        Self {
            backend: fake(),
            store: StateStore::default(),
            runtime: Runtime::default(),
            path: directory.path().join("layout.json"),
            _directory: directory,
        }
    }
    async fn preview(&mut self, proposed: Layout) -> Result<Document> {
        preview_at(
            &self.backend,
            proposed,
            &self.store,
            &self.path,
            &mut self.runtime,
        )
        .await
    }
    async fn finish(&mut self, id: &str, confirm: bool) -> Result<Document> {
        finish_at(
            &self.backend,
            id,
            confirm,
            &self.store,
            &self.path,
            &mut self.runtime,
        )
        .await
    }
    async fn tick(&mut self, wall: u64, monotonic: Instant) -> Result<(Document, bool)> {
        tick_at(
            &self.backend,
            &self.store,
            &self.path,
            &mut self.runtime,
            wall,
            monotonic,
        )
        .await
    }
    async fn document(&self) -> Document {
        load_json_or_default(&self.path, "test").await.unwrap()
    }
}

#[tokio::test]
async fn policy_changes_are_blocked_by_durable_trials_including_after_restart() {
    let mut f = Fixture::new();
    ensure_policy_change_allowed_at(&f.path).await.unwrap();
    let proposed = Layout::observed(&f.backend.outputs.borrow());
    let doc = f.preview(proposed).await.unwrap();
    assert!(ensure_policy_change_allowed_at(&f.path).await.is_err());
    f.runtime = Runtime::default();
    assert!(ensure_policy_change_allowed_at(&f.path).await.is_err());
    f.finish(&doc.trial.unwrap().id, false).await.unwrap();
    ensure_policy_change_allowed_at(&f.path).await.unwrap();
}

#[tokio::test]
async fn replacement_with_same_connector_cannot_be_confirmed_and_rolls_back_early() {
    let mut f = Fixture::new();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    let trial = f.preview(proposed).await.unwrap().trial.unwrap();
    f.backend.outputs.borrow_mut()[0].id += 1;
    let error = f.finish(&trial.id, true).await.unwrap_err();
    assert!(error.to_string().contains("topology changed"));
    let (doc, paused) = f.tick(trial.expires_at - 19, Instant::now()).await.unwrap();
    assert!(!paused);
    assert!(doc.trial.is_none());
    assert_eq!(f.backend.outputs.borrow()[0].scale, 1.25);
}

#[tokio::test]
async fn monotonic_timeout_and_failed_rollback_keep_recovery_intent() {
    let mut f = Fixture::new();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    f.preview(proposed).await.unwrap();
    let expired = Instant::now() + Duration::from_secs(21);
    f.backend.fail.set(true);
    assert!(f.tick(0, expired).await.is_err());
    assert!(ensure_policy_change_allowed_at(&f.path).await.is_err());
    f.backend.fail.set(false);
    let (doc, _) = f.tick(0, expired).await.unwrap();
    assert!(doc.trial.is_none());
    assert_eq!(f.backend.outputs.borrow()[0].scale, 1.25);
}

#[tokio::test]
async fn replacements_are_enabled_before_disabling_and_failure_keeps_the_old_output() {
    for fail in [false, true] {
        let mut f = Fixture::new();
        let mut old = f.backend.outputs.borrow()[0].clone();
        old.name = "DP-1".into();
        let mut replacement = old.clone();
        replacement.id = 1;
        replacement.name = "DP-2".into();
        replacement.disabled = true;
        *f.backend.outputs.borrow_mut() = vec![old, replacement];
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[0].enabled = false;
        proposed.outputs[1].enabled = true;
        f.backend.fail.set(fail);
        assert_eq!(f.preview(proposed).await.is_err(), fail);
        assert!(f.backend.calls.borrow()[0].contains("DP-2"));
        if fail {
            assert_eq!(f.backend.calls.borrow().len(), 1);
            assert!(!f.backend.outputs.borrow()[0].disabled);
        } else {
            assert!(f.backend.outputs.borrow()[0].disabled);
            assert!(!f.backend.outputs.borrow()[1].disabled);
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
        let mut f = Fixture::new();
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[0].scale = 1.5;
        let trial = f.preview(proposed).await.unwrap().trial.unwrap();
        assert_eq!(f.backend.outputs.borrow()[0].scale, 1.5);
        let clock = if recovery == "expiry" {
            trial.expires_at
        } else {
            trial.expires_at - 1
        };
        if recovery == "restart" {
            f.runtime = Runtime::default();
        }
        if recovery == "resume" {
            f.store.record_resume().await;
        }
        let (doc, paused) = f.tick(clock, Instant::now()).await.unwrap();
        assert!(!paused);
        assert!(doc.trial.is_none());
        assert_eq!(f.backend.outputs.borrow()[0].scale, 1.25);
        assert!(f.document().await.trial.is_none());
    }
}

#[tokio::test]
async fn confirmation_is_token_bound_verified_and_persistent() {
    let mut f = Fixture::new();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    let id = f.preview(proposed.clone()).await.unwrap().trial.unwrap().id;
    assert!(f.finish("stale", true).await.is_err());
    f.backend.outputs.borrow_mut()[0].scale = 1.25;
    assert!(f.finish(&id, true).await.is_err());
    f.backend.outputs.borrow_mut()[0].scale = 1.5;
    let doc = f.finish(&id, true).await.unwrap();
    assert_eq!(doc.saved, proposed);
    assert!(doc.trial.is_none());
    assert_eq!(f.document().await, doc);
    assert!(f.finish(&id, true).await.is_err());
}

#[tokio::test]
async fn failed_apply_keeps_rollback_intent_and_sleep_defers_recovery() {
    let mut f = Fixture::new();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    f.backend.fail.set(true);
    assert!(f.preview(proposed).await.is_err());
    assert!(f.document().await.trial.is_some());
    f.backend.calls.borrow_mut().clear();
    f.store.record_sleep_preparation(true).await;
    assert!(f.tick(u64::MAX, Instant::now()).await.unwrap().1);
    assert!(f.backend.calls.borrow().is_empty());
    f.store.record_resume().await;
    f.backend.fail.set(false);
    assert!(
        f.tick(u64::MAX, Instant::now())
            .await
            .unwrap()
            .0
            .trial
            .is_none()
    );
}

#[tokio::test]
async fn saved_layout_waits_for_topology_and_does_not_reapply_working_modes() {
    let mut f = Fixture::new();
    let mut saved = Layout::observed(&f.backend.outputs.borrow());
    saved.outputs[0].scale = 1.5;
    save_json_atomic(&f.path, &Document { saved, trial: None })
        .await
        .unwrap();
    let start = Instant::now();
    assert!(f.tick(0, start).await.unwrap().1);
    assert!(f.backend.calls.borrow().is_empty());
    f.tick(5, start + Duration::from_secs(5)).await.unwrap();
    assert_eq!(f.backend.calls.borrow().len(), 1);
    f.tick(10, start + Duration::from_secs(10)).await.unwrap();
    assert_eq!(f.backend.calls.borrow().len(), 1);
}
