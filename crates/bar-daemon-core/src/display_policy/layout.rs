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
use shelllist_hyprland::monitor::{self, Mode, Position, Rule};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub(crate) mod advanced;
mod draft;
mod identity;
mod outputs;
mod placement;
pub(crate) use identity::Identity;
pub(super) use outputs::serialize_outputs;
pub(crate) use placement::Relative;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative: Option<Relative>,
    #[serde(default)]
    pub match_connector: bool,
    #[serde(default)]
    pub advanced: advanced::Advanced,
}
impl Setting {
    fn validate(&self) -> Result<()> {
        self.advanced.wire().validate()?;
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
    pub(super) fn command(&self) -> Result<shelllist_hyprland::Lua> {
        self.validate()?;
        let rule = if self.enabled {
            Rule::Enable {
                mode: Mode::Exact(&self.mode),
                position: Position::Exact {
                    x: self.x,
                    y: self.y,
                },
                scale: self.scale,
                transform: Some(self.transform),
                mirror: &self.mirror_of,
            }
        } else {
            Rule::Disable
        };
        monitor::command_with_advanced(&self.name, rule, self.advanced.wire())
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
                    && self.transform == output.transform
                    && self.advanced.matches(output)))
    }
    fn observed(output: &Output, outputs: &[Output]) -> Self {
        Self {
            advanced: advanced::Advanced::observed(output),
            identity: Identity::observed(output)
                .filter(|identity| outputs.iter().filter(|o| identity.matches(o)).count() == 1),
            relative: None,
            match_connector: false,
            name: output.name.clone(),
            mode: outputs::current_mode(output),
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
        .any(|prefix| value.starts_with(prefix) && value.len() > prefix.len())
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
                setting.advanced.validate_for(output)?;
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
    pub(crate) fn observed(outputs: &[Output]) -> Self {
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
    pub(crate) fn connected(&self, outputs: &[Output]) -> Result<Self> {
        let mut present = identity::resolve(self, outputs)?;
        for setting in &mut present.outputs {
            if !setting.mirror_of.is_empty() && !outputs.iter().any(|o| o.name == setting.mirror_of)
            {
                setting.mirror_of.clear();
            }
        }
        // An unplugged relative anchor must not prevent emergency recovery.
        let names: HashSet<_> = present
            .outputs
            .iter()
            .filter(|s| s.enabled && s.mirror_of.is_empty())
            .map(|s| s.name.clone())
            .collect();
        for setting in &mut present.outputs {
            if setting
                .relative
                .as_ref()
                .is_some_and(|r| !names.contains(&r.reference))
            {
                setting.relative = None;
            }
        }
        placement::resolve(&mut present)?;
        Ok(present)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trial {
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub automatic: bool,
    pub id: String,
    pub expires_at: u64,
    pub proposed: Layout,
    pub previous: Layout,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    #[serde(default)]
    pub profiles: super::profiles::Catalog,
    // Explicit enablement/content edits override docking policy until it is changed.
    // Old saved layouts retain their existing automatic-policy behaviour.
    #[serde(default)]
    pub manual_enablement: bool,
    pub saved: Layout,
    pub trial: Option<Trial>,
}
impl Document {
    fn restoration(&self, outputs: &[Output]) -> Result<Layout> {
        let mut present = self.saved.connected(outputs)?;
        if !self.manual_enablement {
            // Saved geometry is still useful, but automatic docking alone owns
            // laptop enablement. Never replay a retired manual Off/On first.
            for setting in &mut present.outputs {
                if let Some(output) = outputs
                    .iter()
                    .find(|o| o.name == setting.name && o.internal())
                {
                    setting.enabled = !output.disabled;
                }
            }
            let disabled: HashSet<_> = present
                .outputs
                .iter()
                .filter(|s| !s.enabled)
                .map(|s| s.name.clone())
                .collect();
            for setting in &mut present.outputs {
                if disabled.contains(&setting.mirror_of) {
                    setting.mirror_of.clear();
                }
            }
        }
        Ok(present)
    }
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
pub(super) async fn use_docking_policy() -> Result<Document> {
    use_docking_policy_at(&path()).await
}
async fn use_docking_policy_at(path: &Path) -> Result<Document> {
    let mut doc: Document = load_json_or_default(path, "display layout").await?;
    if doc.manual_enablement {
        doc.manual_enablement = false;
        save_json_atomic(path, &doc).await?;
    }
    Ok(doc)
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
                independent.advanced = Default::default();
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

async fn restore<B: Backend>(backend: &B, previous: &Layout, store: &StateStore) -> Result<()> {
    let present = previous.connected(&backend.outputs().await?)?;
    apply(backend, &present, store).await?;
    let outputs = backend.outputs().await?;
    let present = previous.connected(&outputs)?;
    ensure!(
        present.outputs.iter().all(|s| outputs
            .iter()
            .find(|o| o.name == s.name)
            .is_some_and(|o| s.matches(o, &outputs))),
        "Layout rollback is not yet observed; keeping the recovery journal"
    );
    Ok(())
}

async fn apply_setting<B: Backend>(
    backend: &B,
    setting: &Setting,
    store: &StateStore,
    generation: u64,
) -> Result<()> {
    backend.eligible().await?;
    ensure!(
        !super::sleep_interrupted(store, generation).await,
        "Sleep interrupted display changes"
    );
    let outputs = backend.outputs().await?;
    let Some(output) = outputs.iter().find(|o| o.name == setting.name) else {
        return Ok(());
    };
    ensure!(
        setting
            .identity
            .as_ref()
            .is_none_or(|id| id.matches(output)),
        "Physical display changed before applying settings"
    );
    if setting.enabled {
        setting.advanced.validate_for(output)?;
    }
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preview {
    pub baseline: String,
    #[serde(deserialize_with = "draft::decode")]
    pub outputs: Vec<Setting>,
}

// Process-scoped keyed fingerprint: restarted daemons invalidate old drafts.
// Ignore observation order and transient focus/DPMS fields, but include mode
// catalogs, physical identities, geometry, mirroring and policy ownership.
pub(super) fn baseline(
    outputs: &[Output],
    policy: &super::DisplayPolicy,
    document: &Document,
) -> String {
    use std::{collections::hash_map::RandomState, hash::BuildHasher, sync::OnceLock};
    static HASHER: OnceLock<RandomState> = OnceLock::new();
    let mut ordered: Vec<_> = outputs.iter().filter(|o| connector(&o.name)).collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    let values: Vec<_> = ordered
        .iter()
        .map(|o| {
            let mut modes = o.available_modes.clone();
            modes.sort();
            serde_json::json!([
                o.id,
                o.name,
                o.description,
                Setting::observed(o, outputs),
                modes
            ])
        })
        .collect();
    format!(
        "{:016x}",
        HASHER.get_or_init(RandomState::new).hash_one(
            serde_json::json!([
                policy,
                values,
                document.saved,
                document.profiles.revision,
                document.profiles.active
            ])
            .to_string()
        )
    )
}

pub(super) async fn preview<B: Backend>(
    backend: &B,
    proposed: Preview,
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
    proposed: Preview,
    store: &StateStore,
    path: &Path,
    runtime: &mut Runtime,
) -> Result<Document> {
    backend.eligible().await?;
    super::ensure_docking_settled(store).await?;
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
    let policy = store.read(|s| s.display_policy.policy.clone()).await;
    ensure!(
        proposed.baseline == baseline(&outputs, &policy, &doc),
        "Display baseline is stale; reload the layout"
    );
    let mut proposed = Layout {
        outputs: proposed.outputs,
    };
    // Physical identity is always captured from trusted observation, never a draft.
    for setting in &mut proposed.outputs {
        setting.identity = if setting.match_connector {
            None
        } else {
            outputs
                .iter()
                .find(|o| o.name == setting.name)
                .and_then(Identity::observed)
        };
    }
    // Reject ambiguous persisted bindings while the user can explicitly choose
    // connector matching; recovery must never guess which identical screen moved.
    identity::resolve(&proposed, &outputs)?;
    placement::resolve(&mut proposed)?;
    proposed.validate(&outputs, true)?;
    let mut previous = Layout::observed(&outputs);
    for setting in &mut previous.outputs {
        let requested = proposed.outputs.iter().find(|s| s.name == setting.name);
        setting
            .advanced
            .restrict_to(&requested.map(|s| s.advanced.clone()).unwrap_or_default());
    }
    // Reject a non-recoverable snapshot before touching the compositor.
    previous.validate(&outputs, true)?;
    draft::validate_arrangement(&proposed, &previous)?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    doc.trial = Some(Trial {
        profile: None,
        automatic: false,
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
        ensure!(
            !super::sleep_interrupted(store, runtime.resume).await,
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
        if !trial.automatic {
            doc.profiles.automatic = false;
            doc.profiles.error = None;
        }
        if let Some(name) = trial.profile {
            doc.profiles.active = Some(name);
            if !trial.automatic {
                doc.profiles.automatic = false;
            }
            doc.manual_enablement = true;
        } else if let Some(name) = &doc.profiles.active {
            if let Some(profile) = doc.profiles.items.iter_mut().find(|p| &p.name == name) {
                // Preserve condition identities when a connector was renumbered.
                let old = profile.layout.outputs.clone();
                profile.required = profile
                    .required
                    .iter()
                    .filter_map(|name| {
                        let setting = old.iter().find(|s| &s.name == name)?;
                        doc.saved
                            .outputs
                            .iter()
                            .find(|s| {
                                setting
                                    .identity
                                    .as_ref()
                                    .map_or(s.name == setting.name, |id| {
                                        s.identity.as_ref() == Some(id)
                                    })
                            })
                            .map(|s| s.name.clone())
                    })
                    .collect();
                profile.layout = doc.saved.clone();
                doc.profiles.revision += 1;
            }
            doc.profiles.automatic = false;
        }
    } else {
        restore(backend, &trial.previous, store).await?;
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
pub(super) async fn document() -> Result<Document> {
    load_json_or_default(&path(), "display layout").await
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
    let (preparing, generation) = store
        .read(|s| {
            (
                s.power_sleep.preparing_for_sleep,
                s.power_sleep.resume_generation,
            )
        })
        .await;
    if preparing {
        runtime.trial = None;
        return Ok((doc, true));
    }
    if let Some(trial) = &doc.trial {
        if trial.automatic
            && runtime.trial.as_deref() == Some(&trial.id)
            && runtime.resume == generation
            && now < trial.expires_at
        {
            let outputs = backend.outputs().await?;
            let context = store.display_context.read().await.clone();
            // A changed/ambiguous match invalidates an automatic trial and
            // must enter rollback, even when profile selection itself fails.
            let selected = doc.profiles.select(&outputs, &context).ok().flatten();
            if selected.map(|p| p.name.as_str()) == trial.profile.as_deref()
                && topology(&outputs) == runtime.topology
            {
                if trial.proposed.outputs.iter().all(|s| {
                    outputs
                        .iter()
                        .find(|o| o.name == s.name)
                        .is_some_and(|o| s.matches(o, &outputs))
                }) {
                    let id = trial.id.clone();
                    return Ok((
                        finish_at(backend, &id, true, store, path, runtime).await?,
                        false,
                    ));
                }
            } else {
                runtime.trial = None;
            }
        }
        if runtime.trial.as_deref() == Some(&trial.id)
            && now < trial.expires_at
            && runtime.deadline.is_some_and(|deadline| instant < deadline)
            && runtime.resume == generation
            && topology(&backend.outputs().await?) == runtime.topology
        {
            return Ok((doc, true));
        }
        restore(backend, &trial.previous, store).await?;
        let automatic = trial.automatic;
        if automatic {
            doc.profiles.error = Some("Automatic profile was interrupted or did not reach the requested state; previous layout restored. Edit the profile or toggle automatic switching to retry.".into());
        }
        doc.trial = None;
        save_json_atomic(path, &doc).await?;
        runtime.trial = None;
        runtime.applied = true;
        if automatic && let Some(error) = &doc.profiles.error {
            bail!(error.clone());
        }
        return Ok((doc, false));
    }
    let outputs = backend.outputs().await?;
    let current_topology = topology(&outputs);
    if current_topology != runtime.topology || runtime.resume != generation {
        runtime.topology = current_topology.clone();
        runtime.resume = generation;
        runtime.since = Some(instant);
        runtime.applied = false;
        runtime.error = None;
    }
    if doc.profiles.automatic && !doc.profiles.items.is_empty() {
        let context = store.display_context.read().await.clone();
        let key = serde_json::to_string(&(
            doc.profiles.revision,
            &current_topology,
            &context,
            generation,
        ))?;
        if doc.profiles.attempted.as_deref() != Some(&key)
            && runtime
                .since
                .is_none_or(|since| instant.duration_since(since) >= Duration::from_secs(5))
            && store
                .read(|s| {
                    s.display_policy.docking_state != super::transition::DockingState::Applying
                })
                .await
        {
            doc.profiles.attempted = Some(key);
            doc.profiles.error = None;
            save_json_atomic(path, &doc).await?;
            let attempt: Result<Option<Document>> = async {
                let Some(profile) = doc.profiles.select(&outputs, &context)?.cloned() else {
                    return Ok(None);
                };
                let proposed = profile.resolve(&outputs)?;
                let policy = store.read(|s| s.display_policy.policy.clone()).await;
                let mut result = preview_at(
                    backend,
                    Preview {
                        baseline: baseline(&outputs, &policy, &doc),
                        outputs: proposed.outputs,
                    },
                    store,
                    path,
                    runtime,
                )
                .await?;
                let trial = result.trial.as_mut().context("Profile trial missing")?;
                trial.profile = Some(profile.name);
                trial.automatic = true;
                save_json_atomic(path, &result).await?;
                Ok(Some(result))
            }
            .await;
            match attempt {
                Ok(Some(result)) => return Ok((result, true)),
                Ok(None) => {}
                Err(error) => {
                    let mut failed: Document = load_json_or_default(path, "display layout").await?;
                    failed.profiles.error = Some(format!(
                        "Automatic profile failed: {error:#}. Edit the profile or toggle automatic switching to retry."
                    ));
                    save_json_atomic(path, &failed).await?;
                    return Err(error);
                }
            }
        }
        if let Some(error) = &doc.profiles.error {
            bail!(error.clone());
        }
    }
    let paused = runtime
        .apply_saved(backend, &doc, &outputs, store, instant)
        .await?;
    Ok((doc, paused))
}

pub(super) async fn profile_action<B: Backend>(
    backend: &B,
    request: super::profiles::Request,
    store: &StateStore,
) -> Result<Document> {
    profile_action_at(backend, request, store, &path(), &mut *RUNTIME.lock().await).await
}
async fn profile_action_at<B: Backend>(
    backend: &B,
    request: super::profiles::Request,
    store: &StateStore,
    path: &Path,
    runtime: &mut Runtime,
) -> Result<Document> {
    backend.eligible().await?;
    super::ensure_docking_settled(store).await?;
    ensure!(
        !store.read(|s| s.power_sleep.preparing_for_sleep).await,
        "Cannot edit profiles during sleep preparation"
    );
    let mut doc: Document = load_json_or_default(path, "display layout").await?;
    ensure!(
        doc.trial.is_none(),
        "Finish the layout preview before editing profiles"
    );
    let outputs = backend.outputs().await?;
    if let super::profiles::Request::Preview { name } = request {
        let profile = doc.profiles.get(&name)?;
        let proposed = profile.resolve(&outputs)?;
        let policy = store.read(|s| s.display_policy.policy.clone()).await;
        let mut result = preview_at(
            backend,
            Preview {
                baseline: baseline(&outputs, &policy, &doc),
                outputs: proposed.outputs,
            },
            store,
            path,
            runtime,
        )
        .await?;
        result
            .trial
            .as_mut()
            .context("Profile trial missing")?
            .profile = Some(name);
        save_json_atomic(path, &result).await?;
        return Ok(result);
    }
    let mut current = Layout::observed(&outputs);
    // Metadata edits (especially disabling automation) remain available when
    // saved monitor identities can no longer be resolved unambiguously.
    let configured = doc.saved.connected(&outputs).unwrap_or_default();
    for setting in &mut current.outputs {
        if let Some(saved) = configured.outputs.iter().find(|s| {
            s.name == setting.name
                && s.matches(outputs.iter().find(|o| o.name == s.name).unwrap(), &outputs)
        }) {
            setting.relative.clone_from(&saved.relative);
            setting.match_connector = saved.match_connector;
            setting.identity.clone_from(&saved.identity);
        }
    }
    current.validate(&outputs, true)?;
    let capture = matches!(request, super::profiles::Request::Capture { .. });
    doc.profiles.edit(request, current.clone())?;
    if capture {
        // Capturing selects this layout as the saved manual baseline too;
        // otherwise restart or a subsequent card edit could restore the old
        // profile's geometry or discard captured advanced attributes.
        doc.saved = current;
        doc.manual_enablement = true;
    }
    save_json_atomic(path, &doc).await?;
    Ok(doc)
}

impl Runtime {
    // A saved layout gets one attempt per stable topology/resume. Failure stays
    // visible without repeatedly issuing destructive mode changes.
    async fn apply_saved<B: Backend>(
        &mut self,
        backend: &B,
        doc: &Document,
        outputs: &[Output],
        store: &StateStore,
        instant: Instant,
    ) -> Result<bool> {
        if !self.applied && !doc.saved.outputs.is_empty() {
            if self
                .since
                .is_some_and(|since| instant.duration_since(since) < Duration::from_secs(5))
            {
                return Ok(true);
            }
            self.applied = true;
            let present = match doc.restoration(outputs) {
                Ok(present) => present,
                Err(error) => {
                    self.error = Some(error.to_string());
                    return Err(error);
                }
            };
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
    saved_internal_at(&path(), name).await
}
async fn saved_internal_at(path: &Path, name: &str) -> Result<Option<Setting>> {
    let doc: Document = load_json_or_default(path, "display layout").await?;
    Ok(doc
        .saved
        .outputs
        .into_iter()
        .find(|s| s.name == name)
        .map(|mut setting| {
            // Policy/fallback asked to enable it; retain geometry even if the
            // last manual layout saved this laptop as disabled.
            setting.enabled = true;
            setting.mirror_of.clear();
            setting
        }))
}

#[cfg(test)]
mod tests;
