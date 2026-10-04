//! Sparse, persistent overrides for Hyprland's global focus behaviour.
//! No preferences are changed until explicitly saved; unknown compositor options
//! are omitted instead of presenting guessed defaults as observed state.
use super::{HyprlandClient, POLICY_WRITE, ensure_enabled, layout};
use crate::{
    paths::{data_file, load_json_or_default, save_json_atomic},
    state::StateStore,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shelllist_daemon_core::XdgRoot;
use std::{collections::BTreeMap, path::Path, time::Duration};

pub(super) type Values = BTreeMap<String, Value>;
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int(i64),
    Float(f64),
}
use Kind::{Bool, Float, Int};
const OPTIONS: &[(&str, Kind)] = &[
    ("input:follow_mouse", Int(3)),
    ("misc:mouse_move_focuses_monitor", Bool),
    ("input:mouse_refocus", Bool),
    ("input:follow_mouse_threshold", Float(1000.0)),
    ("input:follow_mouse_shrink", Int(300)),
    ("input:float_switch_override_focus", Int(2)),
    ("misc:always_follow_on_dnd", Bool),
    ("misc:layers_hog_keyboard_focus", Bool),
    ("input:special_fallthrough", Bool),
    ("binds:window_direction_monitor_fallback", Bool),
    ("binds:focus_preferred_method", Int(1)),
    ("binds:movefocus_cycles_fullscreen", Bool),
    ("binds:movefocus_cycles_groupfirst", Bool),
    ("misc:focus_on_activate", Bool),
    ("input:focus_on_close", Int(2)),
    ("misc:on_focus_under_fullscreen", Int(2)),
    ("misc:initial_workspace_tracking", Int(2)),
    ("cursor:no_warps", Bool),
    ("cursor:persistent_warps", Bool),
    ("cursor:warp_on_change_workspace", Int(2)),
    ("cursor:warp_on_toggle_special", Int(2)),
    ("binds:workspace_center_on", Int(1)),
    ("cursor:warp_back_after_non_mouse_input", Bool),
    ("binds:workspace_back_and_forth", Bool),
    ("binds:allow_workspace_cycles", Bool),
    ("binds:hide_special_on_workspace_change", Bool),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct State {
    pub available: bool,
    pub values: Values,
    pub saved: Values,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    saved: Values,
    previous: Values,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    values: Values,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn kind(key: &str) -> Result<Kind> {
    OPTIONS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, kind)| *kind)
        .context("Unknown focus setting")
}
fn valid(key: &str, value: &Value) -> Result<()> {
    let valid = match kind(key)? {
        Bool => value.is_boolean(),
        Int(max) => value.as_i64().is_some_and(|v| (0..=max).contains(&v)),
        Float(max) => value
            .as_f64()
            .is_some_and(|v| v.is_finite() && (0.0..=max).contains(&v)),
    };
    ensure!(valid, "Invalid value for {key}");
    Ok(())
}
fn validate(values: &Values) -> Result<()> {
    for (key, value) in values {
        valid(key, value)?;
    }
    Ok(())
}
fn same(key: &str, actual: &Value, desired: &Value) -> bool {
    if matches!(kind(key), Ok(Float(_))) {
        actual
            .as_f64()
            .zip(desired.as_f64())
            .is_some_and(|(a, b)| (a - b).abs() < 0.0001)
    } else {
        actual == desired
    }
}
fn query() -> String {
    format!(
        "[[BATCH]]{}",
        OPTIONS
            .iter()
            .map(|(key, _)| format!("j/getoption {key}"))
            .collect::<Vec<_>>()
            .join(";")
    )
}
fn parse(reply: &str) -> Result<Values> {
    let parts: Vec<_> = reply.split("\n\n\n").collect();
    ensure!(
        parts.len() == OPTIONS.len(),
        "Incomplete focus settings response"
    );
    let mut values = Values::new();
    for ((key, kind), part) in OPTIONS.iter().zip(parts) {
        let Ok(value) = serde_json::from_str::<Value>(part) else {
            continue;
        };
        if value["option"].as_str() != Some(key) {
            continue;
        }
        let observed = match kind {
            Bool => value
                .get("bool")
                .cloned()
                .or_else(|| match value["int"].as_i64() {
                    Some(0) => Some(Value::Bool(false)),
                    Some(1) => Some(Value::Bool(true)),
                    _ => None,
                }),
            Int(_) => value.get("int").cloned(),
            Float(_) => value.get("float").cloned(),
        };
        if let Some(observed) = observed.filter(|v| valid(key, v).is_ok()) {
            values.insert((*key).into(), observed);
        }
    }
    ensure!(
        !values.is_empty(),
        "This compositor does not expose supported focus settings"
    );
    Ok(values)
}
fn command(values: &Values) -> Result<String> {
    validate(values)?;
    ensure!(!values.is_empty(), "Choose at least one focus setting");
    Ok(format!(
        "eval hl.config({{ {} }})",
        values
            .iter()
            .map(|(key, value)| { format!("[\"{}\"] = {value}", key.replace(':', ".")) })
            .collect::<Vec<_>>()
            .join(", ")
    ))
}
trait Backend {
    async fn eligible(&self) -> Result<()>;
    async fn values(&self) -> Result<Values>;
    async fn configure(&self, values: &Values) -> Result<()>;
}
impl Backend for HyprlandClient {
    async fn eligible(&self) -> Result<()> {
        super::Backend::eligible(self).await
    }
    async fn values(&self) -> Result<Values> {
        parse(&self.request(&query()).await?)
    }
    async fn configure(&self, values: &Values) -> Result<()> {
        let response = self.request(&command(values)?).await?;
        ensure!(
            response.trim() == "ok",
            "Hyprland rejected focus settings: {}",
            response.trim()
        );
        Ok(())
    }
}
async fn guard<B: Backend>(backend: &B, store: &StateStore, generation: u64) -> Result<()> {
    backend.eligible().await?;
    ensure!(
        !super::sleep_interrupted(store, generation).await,
        "Sleep interrupted focus settings"
    );
    Ok(())
}
async fn apply<B: Backend>(
    backend: &B,
    values: &Values,
    store: &StateStore,
    generation: u64,
) -> Result<Values> {
    guard(backend, store, generation).await?;
    backend.configure(values).await?;
    let observed = backend.values().await?;
    ensure!(
        values.iter().all(|(key, value)| observed
            .get(key)
            .is_some_and(|actual| same(key, actual, value))),
        "Compositor has not applied the requested focus settings"
    );
    Ok(observed)
}
async fn change<B: Backend>(
    backend: &B,
    patch: Option<Values>,
    store: &StateStore,
    path: &Path,
) -> Result<State> {
    if let Some(values) = &patch {
        ensure!(!values.is_empty(), "Choose at least one focus setting");
        validate(values)?;
    }
    let mut doc: Document = load_json_or_default(path, "focus settings").await?;
    validate(&doc.saved)?;
    validate(&doc.previous)?;
    let generation = store.read(|s| s.power_sleep.resume_generation).await;
    guard(backend, store, generation).await?;
    let current = backend.values().await?;
    let requested = if let Some(values) = patch {
        for key in values.keys() {
            let previous = current
                .get(key)
                .context("Focus setting is unsupported by this compositor")?;
            doc.previous
                .entry(key.clone())
                .or_insert_with(|| previous.clone());
        }
        doc.saved.extend(values.clone());
        values
    } else {
        // Unsupported settings can be forgotten too; never send them to IPC.
        let restore = doc
            .previous
            .iter()
            .filter(|(key, _)| current.contains_key(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        doc = Document::default();
        restore
    };
    let rollback: Values = requested
        .keys()
        .filter_map(|key| current.get(key).map(|v| (key.clone(), v.clone())))
        .collect();
    let result = async {
        let values = if requested.is_empty() {
            current
        } else {
            apply(backend, &requested, store, generation).await?
        };
        save_json_atomic(path, &doc).await?;
        Ok(State {
            available: true,
            values,
            saved: doc.saved,
            error: None,
        })
    }
    .await;
    if let Err(failure) = &result
        && !rollback.is_empty()
        && let Err(error) = apply(backend, &rollback, store, generation).await
    {
        bail!("{failure:#}; restoring previous focus settings also failed: {error:#}");
    }
    result
}

pub(crate) async fn action(
    method: &str,
    params: Value,
    store: &StateStore,
) -> Result<super::DisplayPolicyState> {
    ensure_enabled()?;
    let patch = match method {
        "displayFocus.set" => Some(serde_json::from_value::<Patch>(params)?.values),
        "displayFocus.reset" => {
            serde_json::from_value::<Empty>(params)?;
            None
        }
        _ => bail!("Unknown focus action"),
    };
    let _guard = POLICY_WRITE.lock().await;
    layout::ensure_policy_change_allowed().await?;
    let focus = tokio::time::timeout(
        Duration::from_secs(8),
        change(
            &HyprlandClient::default(),
            patch,
            store,
            &data_file(XdgRoot::Config, "display-focus.json"),
        ),
    )
    .await
    .context("Focus settings request timed out")??;
    let mut state = store.read(|s| s.display_policy.clone()).await;
    state.focus = focus;
    store.update_display_policy(state.clone()).await;
    Ok(state)
}

async fn reconcile<B: Backend>(backend: &B, store: &StateStore, path: &Path) -> Result<State> {
    let doc: Document = load_json_or_default(path, "focus settings").await?;
    validate(&doc.saved)?;
    let mut values = backend.values().await?;
    let changed: Values = doc
        .saved
        .iter()
        .filter(|(key, value)| {
            values
                .get(*key)
                .is_some_and(|actual| !same(key, actual, value))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if !changed.is_empty() {
        let generation = store.read(|s| s.power_sleep.resume_generation).await;
        values = apply(backend, &changed, store, generation).await?;
    }
    let unsupported = doc.saved.keys().any(|key| !values.contains_key(key));
    Ok(State {
        available: true,
        values,
        saved: doc.saved,
        error: unsupported
            .then(|| "Some saved focus settings are unsupported by this compositor".into()),
    })
}
pub(super) async fn tick(backend: &HyprlandClient, store: &StateStore) -> State {
    match tokio::time::timeout(Duration::from_secs(5), async {
        // The durable trial may be newer than the published state after a
        // failed/timed-out preview. Never rely on UI telemetry for this gate.
        layout::ensure_policy_change_allowed().await?;
        reconcile(
            backend,
            store,
            &data_file(XdgRoot::Config, "display-focus.json"),
        )
        .await
    })
    .await
    {
        Ok(Ok(state)) => state,
        result => State {
            error: Some(match result {
                Ok(Err(error)) => format!("{error:#}"),
                _ => "Focus settings refresh timed out".into(),
            }),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests;
