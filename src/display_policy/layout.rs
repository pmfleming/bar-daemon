//! Saved layouts and durable, time-limited previews. All entry points are
//! serialized with automatic laptop policy by POLICY_WRITE in the parent.
use super::{Backend, Output};
use crate::{
    paths::{data_file, load_json_or_default, save_json_atomic},
    state::StateStore,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::XdgRoot;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const PREVIEW_SECONDS: u64 = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Setting {
    pub name: String,
    pub mode: String,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u8,
    pub enabled: bool,
    /// Empty means an independent (extended) desktop; otherwise a connector.
    #[serde(default)]
    pub mirror_of: String,
}
impl Setting {
    fn validate(&self) -> Result<()> {
        ensure!(connector(&self.name), "Invalid display connector");
        ensure!(mode(&self.mode).is_some(), "Invalid display mode");
        ensure!(
            self.x.abs_diff(0) <= 32768 && self.y.abs_diff(0) <= 32768,
            "Display position is out of bounds"
        );
        ensure!(
            self.scale.is_finite() && (0.5..=4.0).contains(&self.scale),
            "Display scale must be between 0.5 and 4"
        );
        ensure!(self.transform <= 7, "Invalid display rotation");
        ensure!(
            self.mirror_of.is_empty()
                || (connector(&self.mirror_of) && self.mirror_of != self.name),
            "Invalid mirror source"
        );
        Ok(())
    }
    pub(super) fn command(&self) -> Result<String> {
        self.validate()?;
        if !self.enabled {
            return Ok(format!(
                "eval hl.monitor({{ output = \"{}\", disabled = true }})",
                self.name
            ));
        }
        Ok(format!(
            "eval hl.monitor({{ output = \"{}\", mode = \"{}\", position = \"{}x{}\", scale = {}, transform = {}, mirror = \"{}\" }})",
            self.name,
            self.mode.trim_end_matches("Hz"),
            self.x,
            self.y,
            self.scale,
            self.transform,
            self.mirror_of
        ))
    }
    fn observed_mode(output: &Output) -> String {
        format!(
            "{}x{}@{:.2}",
            output.width, output.height, output.refresh_rate
        )
    }
    // Disabled outputs need no geometry match; enabled outputs must agree with
    // the compositor before skipping IPC or accepting a preview confirmation.
    fn matches(&self, output: &Output, outputs: &[Output]) -> bool {
        let mirror_matches = if self.mirror_of.is_empty() {
            !output.mirrored()
        } else {
            output
                .mirror_source(outputs)
                .is_some_and(|source| source.name == self.mirror_of && source.usable())
        };
        self.enabled != output.disabled
            && (!self.enabled
                || (mirror_matches && same_mode(&self.mode, &Self::observed_mode(output))
                    // Hyprland places mirrors at the source's position. Their
                    // own mode/scale/rotation still describe the physical output.
                    && (!self.mirror_of.is_empty() || (self.x == output.x && self.y == output.y))
                    && self.scale == output.scale
                    && self.transform == output.transform))
    }
    fn observed(output: &Output, outputs: &[Output]) -> Self {
        Self {
            name: output.name.clone(),
            mode: if output.disabled && mode(&Self::observed_mode(output)).is_none() {
                output
                    .available_modes
                    .iter()
                    .find(|m| mode(m).is_some())
                    .cloned()
                    .unwrap_or_else(|| Self::observed_mode(output))
            } else {
                Self::observed_mode(output)
            },
            x: output.x,
            y: output.y,
            scale: output.scale,
            transform: output.transform,
            enabled: !output.disabled,
            mirror_of: if !output.disabled && output.mirrored() {
                output
                    .mirror_source(outputs)
                    .map(|source| source.name.clone())
                    .unwrap_or_else(|| output.mirror_of.clone())
            } else {
                String::new()
            },
        }
    }
}

fn connector(value: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-", "DP-", "HDMI-A-"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn mode(value: &str) -> Option<(u32, u32, f64)> {
    let value = value.strip_suffix("Hz").unwrap_or(value);
    if value.len() > 40
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || b"x@.".contains(&c))
    {
        return None;
    }
    let (size, refresh) = value.split_once('@')?;
    let (width, height) = size.split_once('x')?;
    let (width, height, refresh) = (
        width.parse().ok()?,
        height.parse().ok()?,
        refresh.parse::<f64>().ok()?,
    );
    ((1..=16384).contains(&width)
        && (1..=16384).contains(&height)
        && (1.0..=1000.0).contains(&refresh))
    .then_some((width, height, refresh))
}
fn same_mode(left: &str, right: &str) -> bool {
    match (mode(left), mode(right)) {
        (Some((w, h, r)), Some((ww, hh, rr))) => w == ww && h == hh && (r - rr).abs() < 0.1,
        _ => false,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Layout {
    pub outputs: Vec<Setting>,
}
impl Layout {
    fn validate(&self, outputs: &[Output], complete: bool) -> Result<()> {
        ensure!(
            !self.outputs.is_empty() && self.outputs.len() <= 16,
            "Choose between one and sixteen outputs"
        );
        let mut seen = HashSet::new();
        for setting in &self.outputs {
            setting.validate()?;
            ensure!(seen.insert(&setting.name), "Duplicate display connector");
            let output = outputs
                .iter()
                .find(|o| o.name == setting.name)
                .context("Display disconnected; refresh the layout")?;
            if setting.enabled {
                if !setting.mirror_of.is_empty() {
                    ensure!(
                        self.outputs
                            .iter()
                            .any(|source| source.name == setting.mirror_of
                                && source.enabled
                                && source.mirror_of.is_empty()),
                        "Mirror source must be an enabled independent display (no chains or cycles)"
                    );
                }
                ensure!(
                    output
                        .available_modes
                        .iter()
                        .any(|m| same_mode(&setting.mode, m))
                        || same_mode(&setting.mode, &Setting::observed_mode(output)),
                    "Mode is not advertised by this display"
                );
            }
        }
        ensure!(
            self.outputs
                .iter()
                .any(|o| o.enabled && o.mirror_of.is_empty()),
            "Cannot disable every display"
        );
        if complete {
            ensure!(
                outputs
                    .iter()
                    .filter(|o| connector(&o.name))
                    .all(|o| seen.contains(&o.name)),
                "Display topology changed; refresh the layout"
            );
        }
        Ok(())
    }
    fn observed(outputs: &[Output]) -> Self {
        Self {
            outputs: outputs
                .iter()
                .filter(|o| connector(&o.name))
                .map(|output| Setting::observed(output, outputs))
                .collect(),
        }
    }
    // Recovery/hotplug only: promote a surviving mirror when its source is gone.
    // Keep the saved document intact so reconnecting restores the chosen group.
    fn connected(&self, outputs: &[Output]) -> Self {
        let mut present = Self {
            outputs: self
                .outputs
                .iter()
                .filter(|s| outputs.iter().any(|o| o.name == s.name))
                .cloned()
                .collect(),
        };
        for setting in &mut present.outputs {
            if !setting.mirror_of.is_empty() && !outputs.iter().any(|o| o.name == setting.mirror_of)
            {
                setting.mirror_of.clear();
            }
        }
        present
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trial {
    pub id: String,
    pub expires_at: u64,
    pub proposed: Layout,
    pub previous: Layout,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    // Explicit enablement/content edits override docking policy until it is changed.
    // Old saved layouts retain their existing automatic-policy behaviour.
    #[serde(default)]
    pub manual_enablement: bool,
    pub saved: Layout,
    pub trial: Option<Trial>,
}
#[derive(Default)]
pub(super) struct Runtime {
    trial: Option<String>,
    deadline: Option<Instant>,
    topology: Vec<(String, i64)>,
    since: Option<Instant>,
    applied: bool,
    error: Option<String>,
    resume: u64,
}
static RUNTIME: Mutex<Runtime> = Mutex::const_new(Runtime {
    trial: None,
    deadline: None,
    topology: Vec::new(),
    since: None,
    applied: false,
    error: None,
    resume: 0,
});
fn path() -> PathBuf {
    data_file(XdgRoot::Config, "display-layout.json")
}

// Called under POLICY_WRITE: a policy change must not outlive a reverted layout
// or disable the preview's fallback through a competing client.
pub(super) async fn ensure_policy_change_allowed() -> Result<()> {
    ensure_policy_change_allowed_at(&path()).await
}
async fn ensure_policy_change_allowed_at(path: &Path) -> Result<()> {
    let doc: Document = load_json_or_default(path, "display layout").await?;
    ensure!(
        doc.trial.is_none(),
        "Confirm or revert the display layout before changing docking policy"
    );
    Ok(())
}
pub(super) async fn use_docking_policy() -> Result<()> {
    use_docking_policy_at(&path()).await
}
async fn use_docking_policy_at(path: &Path) -> Result<()> {
    let mut doc: Document = load_json_or_default(path, "display layout").await?;
    if doc.manual_enablement {
        doc.manual_enablement = false;
        save_json_atomic(path, &doc).await?;
    }
    Ok(())
}

fn topology(outputs: &[Output]) -> Vec<(String, i64)> {
    let mut keys: Vec<_> = outputs.iter().map(|o| (o.name.clone(), o.id)).collect();
    keys.sort();
    keys
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn apply<B: Backend>(backend: &B, layout: &Layout, store: &StateStore) -> Result<()> {
    let generation = store.read(|s| s.power_sleep.resume_generation).await;
    // Detach changing mirrors before converting their old source into a mirror.
    // This also supports reversing source/target without transient mirror chains.
    let current = backend.outputs().await?;
    for output in current.iter().filter(|o| o.active() && o.mirrored()) {
        if let Some(setting) = layout.outputs.iter().find(|s| s.name == output.name) {
            let old_source = output.mirror_source(&current).map(|s| s.name.as_str());
            if !setting.enabled || old_source != Some(setting.mirror_of.as_str()) {
                let mut independent = Setting::observed(output, &current);
                independent.mirror_of.clear();
                apply_setting(backend, &independent, store, generation).await?;
            }
        }
    }
    // Enable independent sources first, attach mirrors second, disable last.
    let mut ordered = layout.outputs.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|s| {
        if !s.enabled {
            2
        } else {
            u8::from(!s.mirror_of.is_empty())
        }
    });
    for setting in ordered {
        apply_setting(backend, setting, store, generation).await?;
    }
    Ok(())
}

async fn apply_setting<B: Backend>(
    backend: &B,
    setting: &Setting,
    store: &StateStore,
    generation: u64,
) -> Result<()> {
    backend.eligible().await?;
    let sleep = store.snapshot().await.power_sleep;
    ensure!(
        !sleep.preparing_for_sleep && sleep.resume_generation == generation,
        "Sleep interrupted display changes"
    );
    let outputs = backend.outputs().await?;
    let Some(output) = outputs.iter().find(|o| o.name == setting.name) else {
        return Ok(());
    };
    if setting.matches(output, &outputs) {
        return Ok(());
    }
    if !setting.enabled || !setting.mirror_of.is_empty() {
        ensure!(
            !outputs.iter().any(|o| o.active()
                && o.mirror_source(&outputs)
                    .is_some_and(|source| source.name == setting.name)),
            "Detach mirrors before changing their source"
        );
    }
    if setting.enabled && !setting.mirror_of.is_empty() {
        ensure!(
            outputs
                .iter()
                .any(|o| o.name == setting.mirror_of && o.usable()),
            "Mirror source is not usable; keeping the current display"
        );
    }
    if !setting.enabled {
        ensure!(
            outputs.iter().any(|o| o.name != setting.name && o.usable()),
            "No replacement display is usable; keeping fallback"
        );
    }
    backend.configure(setting).await?;
    Ok(())
}

pub(super) async fn preview<B: Backend>(
    backend: &B,
    proposed: Layout,
    store: &StateStore,
) -> Result<Document> {
    preview_at(
        backend,
        proposed,
        store,
        &path(),
        &mut *RUNTIME.lock().await,
    )
    .await
}
async fn preview_at<B: Backend>(
    backend: &B,
    proposed: Layout,
    store: &StateStore,
    path: &Path,
    runtime: &mut Runtime,
) -> Result<Document> {
    backend.eligible().await?;
    ensure!(
        !store.read(|s| s.power_sleep.preparing_for_sleep).await,
        "Cannot preview displays during sleep preparation"
    );
    let mut doc: Document = load_json_or_default(path, "display layout").await?;
    ensure!(
        doc.trial.is_none(),
        "Confirm or revert the previous layout first"
    );
    let outputs = backend.outputs().await?;
    proposed.validate(&outputs, true)?;
    let previous = Layout::observed(&outputs);
    // Reject a non-recoverable snapshot before touching the compositor.
    previous.validate(&outputs, true)?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    doc.trial = Some(Trial {
        id: id.clone(),
        expires_at: now() + PREVIEW_SECONDS,
        proposed: proposed.clone(),
        previous,
    });
    save_json_atomic(path, &doc).await?;
    runtime.trial = Some(id);
    runtime.deadline = Some(Instant::now() + Duration::from_secs(PREVIEW_SECONDS));
    runtime.topology = topology(&outputs);
    runtime.applied = true;
    runtime.error = None;
    runtime.resume = store.read(|s| s.power_sleep.resume_generation).await;
    if let Err(error) = apply(backend, &proposed, store).await {
        // Leave durable rollback intent even if immediate recovery fails.
        runtime.trial = None;
        return Err(error);
    }
    Ok(doc)
}

pub(super) async fn finish<B: Backend>(
    backend: &B,
    id: &str,
    confirm: bool,
    store: &StateStore,
) -> Result<Document> {
    finish_at(
        backend,
        id,
        confirm,
        store,
        &path(),
        &mut *RUNTIME.lock().await,
    )
    .await
}

async fn finish_at<B: Backend>(
    backend: &B,
    id: &str,
    confirm: bool,
    store: &StateStore,
    path: &Path,
    runtime: &mut Runtime,
) -> Result<Document> {
    let mut doc: Document = load_json_or_default(path, "display layout").await?;
    let trial = doc.trial.take().context("No layout preview is pending")?;
    ensure!(trial.id == id, "This layout preview is stale");
    backend.eligible().await?;
    if confirm {
        ensure!(
            runtime.trial.as_deref() == Some(id)
                && now() < trial.expires_at
                && runtime
                    .deadline
                    .is_some_and(|deadline| Instant::now() < deadline),
            "Layout confirmation expired"
        );
        let sleep = store.snapshot().await.power_sleep;
        ensure!(
            !sleep.preparing_for_sleep && sleep.resume_generation == runtime.resume,
            "Sleep interrupted the preview"
        );
        let outputs = backend.outputs().await?;
        ensure!(
            topology(&outputs) == runtime.topology,
            "Display topology changed during the preview"
        );
        trial.proposed.validate(&outputs, true)?;
        ensure!(
            trial.proposed.outputs.iter().all(|s| outputs
                .iter()
                .find(|o| o.name == s.name)
                .is_some_and(|o| s.matches(o, &outputs))),
            "Compositor has not applied the requested layout"
        );
        doc.manual_enablement |= trial.proposed.outputs.iter().any(|setting| {
            trial.previous.outputs.iter().any(|previous| {
                previous.name == setting.name
                    && (previous.enabled != setting.enabled
                        || previous.mirror_of != setting.mirror_of)
            })
        });
        doc.saved = trial.proposed;
    } else {
        apply(
            backend,
            &trial.previous.connected(&backend.outputs().await?),
            store,
        )
        .await?;
    }
    save_json_atomic(path, &doc).await?;
    runtime.trial = None;
    runtime.applied = true;
    runtime.error = None;
    Ok(doc)
}

pub(super) async fn tick<B: Backend>(backend: &B, store: &StateStore) -> Result<(Document, bool)> {
    tick_at(
        backend,
        store,
        &path(),
        &mut *RUNTIME.lock().await,
        now(),
        Instant::now(),
    )
    .await
}
async fn tick_at<B: Backend>(
    backend: &B,
    store: &StateStore,
    path: &Path,
    runtime: &mut Runtime,
    now: u64,
    instant: Instant,
) -> Result<(Document, bool)> {
    let mut doc: Document = load_json_or_default(path, "display layout").await?;
    let sleep = store.snapshot().await.power_sleep;
    if sleep.preparing_for_sleep {
        runtime.trial = None;
        return Ok((doc, true));
    }
    if let Some(trial) = &doc.trial {
        if runtime.trial.as_deref() == Some(&trial.id)
            && now < trial.expires_at
            && runtime.deadline.is_some_and(|deadline| instant < deadline)
            && runtime.resume == sleep.resume_generation
            && topology(&backend.outputs().await?) == runtime.topology
        {
            return Ok((doc, true));
        }
        apply(
            backend,
            &trial.previous.connected(&backend.outputs().await?),
            store,
        )
        .await?;
        doc.trial = None;
        save_json_atomic(path, &doc).await?;
        runtime.trial = None;
        runtime.applied = true;
        return Ok((doc, false));
    }
    let outputs = backend.outputs().await?;
    let current_topology = topology(&outputs);
    if current_topology != runtime.topology || runtime.resume != sleep.resume_generation {
        runtime.topology = current_topology;
        runtime.resume = sleep.resume_generation;
        runtime.since = Some(instant);
        runtime.applied = false;
        runtime.error = None;
    }
    let paused = runtime
        .apply_saved(backend, &doc.saved, &outputs, store, instant)
        .await?;
    Ok((doc, paused))
}

impl Runtime {
    // A saved layout gets one attempt per stable topology/resume. Failure stays
    // visible without repeatedly issuing destructive mode changes.
    async fn apply_saved<B: Backend>(
        &mut self,
        backend: &B,
        saved: &Layout,
        outputs: &[Output],
        store: &StateStore,
        instant: Instant,
    ) -> Result<bool> {
        if !self.applied && !saved.outputs.is_empty() {
            if self
                .since
                .is_some_and(|since| instant.duration_since(since) < Duration::from_secs(5))
            {
                return Ok(true);
            }
            self.applied = true;
            let present = saved.connected(outputs);
            // A saved external-only layout cannot be applied while its enabled
            // outputs are unplugged. Let reconciliation restore the laptop.
            if present.outputs.iter().any(|s| s.enabled) {
                let result = async {
                    present.validate(outputs, false)?;
                    apply(backend, &present, store).await
                }
                .await;
                self.error = result.err().map(|error| format!(
                    "Saved layout was not applied: {error:#}. Preview a corrected layout to retry."
                ));
            }
        }
        if let Some(error) = &self.error {
            bail!(error.clone())
        }
        Ok(false)
    }
}

pub(super) async fn saved_internal(name: &str) -> Result<Option<Setting>> {
    let doc: Document = load_json_or_default(&path(), "display layout").await?;
    Ok(doc
        .saved
        .outputs
        .into_iter()
        .find(|s| s.name == name && s.enabled)
        .map(|mut setting| {
            setting.mirror_of.clear();
            setting
        }))
}

#[cfg(test)]
mod tests;
