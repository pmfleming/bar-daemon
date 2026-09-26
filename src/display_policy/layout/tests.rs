use super::{
    Backend, Document, Layout, Output, Runtime, Setting, ensure_policy_change_allowed_at,
    finish_at, mode, preview_at, tick_at,
};
use crate::{
    paths::{load_json_or_default, save_json_atomic},
    state::StateStore,
};
use anyhow::{Context, Result, bail};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    time::{Duration, Instant},
};

mod mirroring;

struct Fake {
    outputs: RefCell<Vec<Output>>,
    calls: RefCell<Vec<String>>,
    fail: Cell<bool>,
    ignore_mirror: Cell<bool>,
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
        let source = if setting.enabled && !setting.mirror_of.is_empty() {
            let source = outputs
                .iter()
                .find(|o| o.name == setting.mirror_of && o.usable())
                .context("mirror source unavailable")?
                .clone();
            if outputs.iter().any(|o| {
                o.active()
                    && o.mirror_source(&outputs)
                        .is_some_and(|parent| parent.name == setting.name)
            }) {
                bail!("cannot create a transient mirror chain");
            }
            Some(source)
        } else {
            None
        };
        let output = outputs.iter_mut().find(|o| o.name == setting.name).unwrap();
        output.disabled = !setting.enabled;
        let (width, height, refresh) = mode(&setting.mode).unwrap();
        output.width = width;
        output.height = height;
        output.refresh_rate = refresh;
        output.x = source.as_ref().map_or(setting.x, |s| s.x);
        output.y = source.as_ref().map_or(setting.y, |s| s.y);
        if !self.ignore_mirror.get() {
            output.mirror_of = source.map_or_else(String::new, |s| s.id.to_string());
        }
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
        ignore_mirror: Cell::new(false),
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
async fn replacements_are_enabled_before_disabling_and_failure_keeps_the_old_output() {
    for (name, fail) in [
        ("eDP-1", false),
        ("eDP-1", true),
        ("DP-1", false),
        ("DP-1", true),
    ] {
        let mut f = Fixture::new();
        let mut old = f.backend.outputs.borrow()[0].clone();
        old.name = name.into();
        let mut replacement = old.clone();
        replacement.id = 1;
        replacement.name = "DP-2".into();
        replacement.disabled = true;
        *f.backend.outputs.borrow_mut() = vec![old, replacement];
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[0].enabled = false;
        proposed.outputs[1].enabled = true;
        f.backend.fail.set(fail);
        let preview = f.preview(proposed).await;
        assert_eq!(preview.is_err(), fail);
        assert!(f.backend.calls.borrow()[0].contains("DP-2"));
        if fail {
            assert_eq!(f.backend.calls.borrow().len(), 1);
            assert!(!f.backend.outputs.borrow()[0].disabled);
        } else {
            assert!(f.backend.outputs.borrow()[0].disabled);
            assert!(!f.backend.outputs.borrow()[1].disabled);
            // Disabled geometry is irrelevant when confirming the active replacement.
            f.backend.outputs.borrow_mut()[0].width = 0;
            f.finish(&preview.unwrap().trial.unwrap().id, true)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn any_output_can_be_disabled_but_not_the_last_and_rollback_preserves_enablement() {
    for disabled in [0, 1] {
        let mut f = Fixture::new();
        let mut external = f.backend.outputs.borrow()[0].clone();
        external.name = "DP-1".into();
        external.id = 1;
        f.backend.outputs.borrow_mut().push(external);
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[disabled].enabled = false;
        let trial = f.preview(proposed.clone()).await.unwrap().trial.unwrap();
        assert!(f.backend.outputs.borrow()[disabled].disabled);
        let doc = f.finish(&trial.id, true).await.unwrap();
        assert!(doc.manual_enablement);
        assert_eq!(doc.saved, proposed);
        // Disabled panels may report no current geometry. Use an advertised
        // mode for recovery without silently turning them on in the snapshot.
        f.backend.outputs.borrow_mut()[disabled].width = 0;
        let previous = Layout::observed(&f.backend.outputs.borrow());
        assert!(!previous.outputs[disabled].enabled);
        assert!(previous.validate(&f.backend.outputs.borrow(), true).is_ok());
        let mut all_off = previous.clone();
        all_off.outputs[1 - disabled].enabled = false;
        f.backend.calls.borrow_mut().clear();
        assert!(
            f.preview(all_off)
                .await
                .unwrap_err()
                .to_string()
                .contains("Cannot disable every")
        );
        assert!(f.backend.calls.borrow().is_empty());
        let mut replacement = previous.clone();
        replacement.outputs[disabled].enabled = true;
        let trial = f.preview(replacement).await.unwrap().trial.unwrap();
        assert_eq!(trial.previous, previous);
        f.finish(&trial.id, false).await.unwrap();
        assert!(f.backend.outputs.borrow()[disabled].disabled);
        assert!(!f.backend.outputs.borrow()[1 - disabled].disabled);
        // Returning to docking policy preserves saved geometry but relinquishes
        // the explicit enablement override durably.
        super::use_docking_policy_at(&f.path).await.unwrap();
        let resumed = f.document().await;
        assert!(!resumed.manual_enablement);
        assert_eq!(resumed.saved, doc.saved);
    }
}

#[tokio::test]
async fn external_only_saved_layout_defers_to_fallback_when_undocked() {
    let mut f = Fixture::new();
    f.backend.outputs.borrow_mut()[0].disabled = true;
    let mut saved = Layout::observed(&f.backend.outputs.borrow());
    let mut external = saved.outputs[0].clone();
    external.name = "DP-1".into();
    external.enabled = true;
    saved.outputs.push(external);
    save_json_atomic(
        &f.path,
        &Document {
            saved,
            manual_enablement: true,
            trial: None,
        },
    )
    .await
    .unwrap();
    let start = Instant::now();
    f.tick(0, start).await.unwrap();
    let (doc, paused) = f.tick(5, start + Duration::from_secs(5)).await.unwrap();
    assert!(!paused);
    assert!(doc.manual_enablement);
    assert!(f.backend.calls.borrow().is_empty());
    // Reconciliation owns the emergency fallback; no saved-layout error blocks it.
    let old: Document = serde_json::from_str(r#"{"saved":{"outputs":[]},"trial":null}"#).unwrap();
    assert!(!old.manual_enablement);
}

#[tokio::test]
async fn disable_rechecks_that_a_replacement_is_actually_usable() {
    for unusable in ["disabled", "zero-size", "disconnected"] {
        let f = Fixture::new();
        let mut external = f.backend.outputs.borrow()[0].clone();
        external.name = "DP-1".into();
        f.backend.outputs.borrow_mut().push(external);
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[0].enabled = false;
        // Model a replacement disappearing after layout validation and before
        // the disable command (only the disable remains to be applied).
        proposed.outputs.remove(1);
        match unusable {
            "disabled" => f.backend.outputs.borrow_mut()[1].disabled = true,
            "zero-size" => f.backend.outputs.borrow_mut()[1].width = 0,
            _ => {
                f.backend.outputs.borrow_mut().pop();
            }
        }
        assert!(super::apply(&f.backend, &proposed, &f.store).await.is_err());
        assert!(f.backend.calls.borrow().is_empty());
        assert!(!f.backend.outputs.borrow()[0].disabled);
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
    for recovery in ["expiry", "monotonic", "restart", "resume"] {
        let mut f = Fixture::new();
        let mut proposed = Layout::observed(&f.backend.outputs.borrow());
        proposed.outputs[0].scale = 1.5;
        let trial = f.preview(proposed).await.unwrap().trial.unwrap();
        assert_eq!(f.backend.outputs.borrow()[0].scale, 1.5);
        assert!(ensure_policy_change_allowed_at(&f.path).await.is_err());
        let (clock, monotonic) = match recovery {
            "expiry" => (trial.expires_at, Instant::now()),
            "monotonic" => (0, Instant::now() + Duration::from_secs(21)),
            _ => (trial.expires_at - 1, Instant::now()),
        };
        if recovery == "restart" {
            f.runtime = Runtime::default();
            assert!(ensure_policy_change_allowed_at(&f.path).await.is_err());
        }
        if recovery == "resume" {
            f.store.record_resume().await;
        }
        if recovery == "monotonic" {
            f.backend.fail.set(true);
            assert!(f.tick(clock, monotonic).await.is_err());
            assert!(f.document().await.trial.is_some());
            assert!(ensure_policy_change_allowed_at(&f.path).await.is_err());
            f.backend.fail.set(false);
        }
        let (doc, paused) = f.tick(clock, monotonic).await.unwrap();
        assert!(!paused);
        assert!(doc.trial.is_none());
        assert_eq!(f.backend.outputs.borrow()[0].scale, 1.25);
        assert!(f.document().await.trial.is_none());
        ensure_policy_change_allowed_at(&f.path).await.unwrap();
    }
}

#[tokio::test]
async fn confirmation_is_token_bound_verified_and_persistent() {
    let mut f = Fixture::new();
    let mut proposed = Layout::observed(&f.backend.outputs.borrow());
    proposed.outputs[0].scale = 1.5;
    let id = f.preview(proposed.clone()).await.unwrap().trial.unwrap().id;
    assert!(f.finish("stale", true).await.is_err());
    let actual = f.backend.outputs.borrow()[0].clone();
    let changes: [fn(&mut Output); 8] = [
        |o| o.width += 1,
        |o| o.height += 1,
        |o| o.refresh_rate += 0.2,
        |o| o.x += 1,
        |o| o.y += 1,
        |o| o.scale += 0.25,
        |o| o.transform = 1,
        |o| o.disabled = true,
    ];
    for change in changes {
        change(&mut f.backend.outputs.borrow_mut()[0]);
        assert!(f.finish(&id, true).await.is_err());
        f.backend.outputs.borrow_mut()[0] = actual.clone();
    }
    let doc = f.finish(&id, true).await.unwrap();
    assert_eq!(doc.saved, proposed);
    assert!(doc.trial.is_none());
    assert_eq!(f.document().await, doc);
    assert!(f.finish(&id, true).await.is_err());
    let mut replacement = proposed.clone();
    replacement.outputs[0].scale = 2.0;
    let trial = f.preview(replacement).await.unwrap().trial.unwrap();
    let cancelled = f.finish(&trial.id, false).await.unwrap();
    assert!(cancelled.trial.is_none());
    assert_eq!(cancelled.saved, proposed);
    assert_eq!(f.backend.outputs.borrow()[0].scale, 1.5);
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
    save_json_atomic(
        &f.path,
        &Document {
            saved,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let start = Instant::now();
    assert!(f.tick(0, start).await.unwrap().1);
    assert!(f.backend.calls.borrow().is_empty());
    f.tick(5, start + Duration::from_secs(5)).await.unwrap();
    assert_eq!(f.backend.calls.borrow().len(), 1);
    f.tick(10, start + Duration::from_secs(10)).await.unwrap();
    assert_eq!(f.backend.calls.borrow().len(), 1);

    // A failed saved-layout attempt remains visible, without destructive retries.
    f.backend.outputs.borrow_mut()[0].scale = 1.25;
    f.backend.fail.set(true);
    f.store.record_resume().await;
    assert!(f.tick(15, start + Duration::from_secs(15)).await.unwrap().1);
    for seconds in [20, 25] {
        let error = f
            .tick(seconds, start + Duration::from_secs(seconds))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Saved layout was not applied"));
        assert_eq!(f.backend.calls.borrow().len(), 2);
    }
    // A new resume resets both the failure and its one-attempt guard.
    f.backend.fail.set(false);
    f.store.record_resume().await;
    assert!(f.tick(30, start + Duration::from_secs(30)).await.unwrap().1);
    assert!(!f.tick(35, start + Duration::from_secs(35)).await.unwrap().1);
    assert_eq!(f.backend.calls.borrow().len(), 3);
    assert_eq!(f.backend.outputs.borrow()[0].scale, 1.5);
}
