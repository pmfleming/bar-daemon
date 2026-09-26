//! Opt-in laptop display policy. Nix enables ownership; user preferences and
//! wake/hotplug reconciliation live here, not in a competing shell script.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shelllist_daemon_core::XdgRoot;
use tokio::sync::Mutex;

use crate::{
    hyprland::HyprlandClient,
    paths::{data_file, load_json_or_default, save_json_atomic},
    state::StateStore,
};
use planner::{Output, Planner, external_signature};

pub(crate) mod focus;
pub(crate) mod layout;
mod planner;
#[cfg(test)]
mod tests;

static POLICY_WRITE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplayPolicy {
    pub prefer_external: bool,
}
impl Default for DisplayPolicy {
    fn default() -> Self {
        Self {
            prefer_external: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct DisplayPolicyState {
    pub available: bool,
    pub policy: DisplayPolicy,
    pub status: String,
    pub error: Option<String>,
    pub outputs: Vec<Output>,
    pub layout: layout::Document,
    #[serde(default)]
    pub focus: focus::State,
}

fn enabled() -> bool {
    std::env::var("BAR_DAEMON_DISPLAY_CONTROL").is_ok_and(|v| v == "1")
}
fn policy_path() -> PathBuf {
    data_file(XdgRoot::Config, "displays.json")
}
async fn load() -> Result<DisplayPolicy> {
    load_json_or_default(&policy_path(), "display policy").await
}

pub(crate) async fn set(policy: DisplayPolicy, store: &StateStore) -> Result<DisplayPolicyState> {
    if !enabled() {
        bail!("Enable programs.shelllist.displays.enable to manage laptop displays");
    }
    let _guard = POLICY_WRITE.lock().await;
    layout::ensure_policy_change_allowed().await?;
    save_json_atomic(&policy_path(), &policy).await?;
    layout::use_docking_policy().await?;
    let state = DisplayPolicyState {
        available: true,
        policy,
        status: "pending".into(),
        error: None,
        ..store.snapshot().await.display_policy
    };
    store.update_display_policy(state.clone()).await;
    Ok(state)
}

pub(crate) async fn layout_action(
    action: &str,
    params: serde_json::Value,
    store: &StateStore,
) -> Result<DisplayPolicyState> {
    ensure_enabled()?;
    let _guard = POLICY_WRITE.lock().await;
    let backend = HyprlandClient::default();
    let document = tokio::time::timeout(Duration::from_secs(8), async {
        if action == "displayLayout.preview" {
            layout::preview(&backend, serde_json::from_value(params)?, store).await
        } else {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Token {
                id: String,
            }
            let token: Token = serde_json::from_value(params)?;
            layout::finish(
                &backend,
                &token.id,
                action == "displayLayout.confirm",
                store,
            )
            .await
        }
    })
    .await
    .context("Display layout request timed out; unconfirmed changes will revert")??;
    let mut state = store.snapshot().await.display_policy;
    state.layout = document;
    state.outputs = backend.outputs().await?;
    state.error = None;
    store.update_display_policy(state.clone()).await;
    Ok(state)
}

fn ensure_enabled() -> Result<()> {
    if !enabled() {
        bail!("Enable programs.shelllist.displays.enable to manage displays")
    }
    Ok(())
}

trait Backend {
    async fn eligible(&self) -> Result<()>;
    async fn outputs(&self) -> Result<Vec<Output>>;
    async fn apply(&self, output: &Output, disable: bool) -> Result<()>;
    async fn configure(&self, _setting: &layout::Setting) -> Result<()> {
        bail!("Layout control is unavailable")
    }
}

impl Backend for HyprlandClient {
    async fn eligible(&self) -> Result<()> {
        ensure_no_legacy_owner().await?;
        let connection = zbus::Connection::system().await?;
        let session = crate::sleep::current_session(&connection).await?;
        let active: bool = session.get_property("Active").await?;
        let remote: bool = session.get_property("Remote").await?;
        let kind: String = session.get_property("Type").await?;
        if !active || remote || kind != "wayland" {
            bail!("display control is waiting for the active local Wayland session");
        }
        Ok(())
    }
    async fn outputs(&self) -> Result<Vec<Output>> {
        serde_json::from_str(&self.request("j/monitors all").await?)
            .context("read Hyprland display topology")
    }
    async fn configure(&self, setting: &layout::Setting) -> Result<()> {
        let response = self.request(&setting.command()?).await?;
        if response.trim() != "ok" {
            bail!("Hyprland rejected layout change: {}", response.trim());
        }
        Ok(())
    }
    async fn apply(&self, output: &Output, disable: bool) -> Result<()> {
        if !disable {
            if let Ok(Some(setting)) = layout::saved_internal(&output.name).await {
                if self.configure(&setting).await.is_ok() {
                    return Ok(());
                }
            }
        }
        let response = self.request(&output.command(disable)?).await?;
        if response.trim() != "ok" {
            bail!("Hyprland rejected display change: {}", response.trim());
        }
        Ok(())
    }
}

async fn ensure_no_legacy_owner() -> Result<()> {
    let connection = zbus::Connection::session().await?;
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    )
    .await?;
    let path: zvariant::OwnedObjectPath = match manager
        .call("GetUnit", &("hypr-monitor-auto.service",))
        .await
    {
        Ok(path) => path,
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit" =>
        {
            return Ok(());
        }
        Err(error) => return Err(error).context("check legacy monitor policy ownership"),
    };
    let unit = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        path,
        "org.freedesktop.systemd1.Unit",
    )
    .await?;
    let active: String = unit.get_property("ActiveState").await?;
    if !matches!(active.as_str(), "inactive" | "failed") {
        bail!(
            "hypr-monitor-auto.service still owns display policy; remove the old service before enabling daemon control"
        );
    }
    Ok(())
}

async fn reconcile<B: Backend>(
    backend: &B,
    planner: &mut Planner,
    policy: &DisplayPolicy,
    preserve_enablement: bool,
    store: &StateStore,
    now: Instant,
) -> Result<&'static str> {
    let sleep = store.snapshot().await.power_sleep;
    if sleep.preparing_for_sleep {
        planner.reset();
        return Ok("sleeping");
    }
    backend.eligible().await?;
    let outputs = backend.outputs().await?;
    // Layout previews and explicit enablement choices must not be undone by
    // docking policy. Still restore the laptop if the last usable output goes.
    if preserve_enablement && outputs.iter().any(Output::usable) {
        planner.reset();
        return Ok("layout");
    }
    let plan = planner.plan(policy.prefer_external, &outputs, now);
    for target in &plan.targets {
        if plan.disable_internal {
            // Never disable the fallback based on the snapshot that selected
            // the plan. Hotplug can invalidate it while earlier IPC is pending.
            backend.eligible().await?;
            let current = backend.outputs().await?;
            if external_signature(&current) != plan.external
                || !current
                    .iter()
                    .any(|o| o.name == target.name && o.id == target.id)
                || current.iter().any(|o| {
                    o.active()
                        && o.mirror_source(&current)
                            .is_some_and(|source| source.name == target.name)
                })
            {
                bail!(
                    "display topology changed before disabling the laptop screen; keeping the fallback"
                );
            }
        }
        let current_sleep = store.snapshot().await.power_sleep;
        if current_sleep.preparing_for_sleep
            || current_sleep.resume_generation != sleep.resume_generation
        {
            bail!("sleep transition interrupted display reconciliation");
        }
        backend.apply(target, plan.disable_internal).await?;
    }
    Ok(plan.status)
}

pub(crate) async fn monitor(store: StateStore) {
    if !enabled() {
        store
            .update_display_policy(DisplayPolicyState {
                status: "disabled".into(),
                error: Some(
                    "Enable programs.shelllist.displays.enable to manage laptop displays".into(),
                ),
                ..Default::default()
            })
            .await;
        return;
    }
    let backend = HyprlandClient::default();
    let mut planner = Planner::default();
    let mut last_resume = 0;
    let mut events = store.subscribe();
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = timer.tick() => {},
            event = events.recv() => match event {
                Ok(event) if event.stream == crate::protocol::stream::POWER_SLEEP => {
                    let power = store.snapshot().await.power_sleep;
                    if power.resume_generation == last_resume && !power.preparing_for_sleep { continue; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                _ => continue,
            }
        }
        let _guard = POLICY_WRITE.lock().await;
        let resume = store.snapshot().await.power_sleep.resume_generation;
        if resume != last_resume {
            planner.reset();
            last_resume = resume;
        }
        let mut state = DisplayPolicyState {
            available: true,
            error: None,
            ..store.snapshot().await.display_policy
        };
        let result = async {
            state.policy = load().await?;
            tokio::time::timeout(Duration::from_secs(8), async {
                let layout_result = layout::tick(&backend, &store).await;
                let paused = layout_result.as_ref().map_or(true, |(_, paused)| *paused);
                let mut policy = state.policy.clone();
                if paused {
                    policy.prefer_external = false;
                }
                let preserve_enablement = layout_result
                    .as_ref()
                    .is_ok_and(|(doc, _)| doc.trial.is_some() || doc.manual_enablement);
                let status = reconcile(
                    &backend,
                    &mut planner,
                    &policy,
                    preserve_enablement,
                    &store,
                    Instant::now(),
                )
                .await?;
                state.outputs = backend.outputs().await?;
                let (layout, _) = layout_result?;
                state.layout = layout;
                Ok::<_, anyhow::Error>(if paused { "layout-preview" } else { status })
            })
            .await
            .context("display reconciliation timed out")?
        }
        .await;
        match result {
            Ok(status) => state.status = status.into(),
            Err(error) => {
                planner.reset();
                state.status = "error".into();
                state.error = Some(format!("{error:#}"));
            }
        }
        if state.layout.trial.is_none() && !store.read(|s| s.power_sleep.preparing_for_sleep).await
        {
            state.focus = focus::tick(&backend, &store).await;
        }
        let previous = store.snapshot().await.display_policy;
        if state.status != previous.status || state.error != previous.error {
            tracing::info!(status = %state.status, error = ?state.error, "laptop display policy");
        }
        store.update_display_policy(state).await;
    }
}
