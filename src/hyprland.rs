use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::sleep,
};

use crate::{
    model::{MonitorState, Workspace, WorkspaceState},
    state::StateStore,
};

#[derive(Debug, Deserialize)]
struct HyprWorkspace {
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    monitor: String,
    #[serde(default)]
    windows: u32,
    #[serde(default)]
    urgent: bool,
    #[serde(default, rename = "hasfullscreen")]
    has_fullscreen: bool,
    #[serde(default, rename = "lastwindowtitle")]
    last_window_title: String,
}

#[derive(Debug, Deserialize)]
struct HyprMonitor {
    id: i64,
    name: String,
    #[serde(default)]
    focused: bool,
    #[serde(default, rename = "activeWorkspace")]
    active_workspace: HyprActiveWorkspace,
}

#[derive(Debug, Default, Deserialize)]
struct HyprActiveWorkspace {
    #[serde(default)]
    id: i64,
}

#[derive(Debug, Clone)]
pub struct HyprlandClient {
    runtime_dir: PathBuf,
    signature: Option<String>,
}

impl Default for HyprlandClient {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl HyprlandClient {
    pub fn from_environment() -> Self {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
        Self {
            runtime_dir,
            signature: env::var("HYPRLAND_INSTANCE_SIGNATURE")
                .ok()
                .filter(|v| !v.is_empty()),
        }
    }

    #[cfg(test)]
    fn with_runtime(runtime_dir: PathBuf, signature: Option<String>) -> Self {
        Self {
            runtime_dir,
            signature,
        }
    }

    async fn instance_dir(&self) -> Result<PathBuf> {
        let root = self.runtime_dir.join("hypr");
        if let Some(signature) = &self.signature {
            let path = root.join(signature);
            if fs::try_exists(path.join(".socket.sock"))
                .await
                .unwrap_or(false)
            {
                return Ok(path);
            }
        }
        let mut entries = fs::read_dir(&root)
            .await
            .with_context(|| format!("read Hyprland runtime directory {}", root.display()))?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if fs::try_exists(path.join(".socket.sock"))
                .await
                .unwrap_or(false)
            {
                return Ok(path);
            }
        }
        bail!(
            "no active Hyprland IPC instance found under {}",
            root.display()
        )
    }

    async fn request(&self, command: &str) -> Result<String> {
        let socket = self.instance_dir().await?.join(".socket.sock");
        request_socket(&socket, command).await
    }

    pub async fn snapshot(&self) -> Result<WorkspaceState> {
        let (workspaces, monitors) =
            tokio::try_join!(self.request("j/workspaces"), self.request("j/monitors"))?;
        parse_snapshot(&workspaces, &monitors)
    }

    pub async fn focus_workspace(&self, workspace_id: i64, on_current_monitor: bool) -> Result<()> {
        if workspace_id <= 0 {
            bail!("workspace_id must be a positive integer");
        }
        let current = if on_current_monitor {
            ", on_current_monitor = true"
        } else {
            ""
        };
        let command = format!("dispatch hl.dsp.focus({{ workspace = {workspace_id}{current} }})");
        let response = self.request(&command).await?;
        if response.trim_start().starts_with("ok") {
            Ok(())
        } else {
            bail!("Hyprland rejected workspace focus: {}", response.trim())
        }
    }

    async fn event_socket(&self) -> Result<UnixStream> {
        let socket = self.instance_dir().await?.join(".socket2.sock");
        UnixStream::connect(&socket)
            .await
            .with_context(|| format!("connect to Hyprland event socket {}", socket.display()))
    }
}

async fn request_socket(path: &Path, command: &str) -> Result<String> {
    let mut stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connect to Hyprland command socket {}", path.display()))?;
    stream.write_all(command.as_bytes()).await?;
    stream.shutdown().await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    Ok(response)
}

fn parse_snapshot(workspaces_json: &str, monitors_json: &str) -> Result<WorkspaceState> {
    let mut workspaces: Vec<HyprWorkspace> =
        serde_json::from_str(workspaces_json).context("decode Hyprland workspaces")?;
    let mut monitors: Vec<HyprMonitor> =
        serde_json::from_str(monitors_json).context("decode Hyprland monitors")?;
    workspaces.sort_by_key(|workspace| workspace.id);
    monitors.sort_by_key(|monitor| monitor.id);
    let focused_monitor = monitors
        .iter()
        .find(|monitor| monitor.focused)
        .map(|monitor| monitor.name.clone());
    Ok(WorkspaceState {
        available: true,
        focused_monitor,
        monitors: monitors
            .into_iter()
            .map(|monitor| MonitorState {
                id: monitor.id,
                name: monitor.name,
                focused: monitor.focused,
                active_workspace_id: monitor.active_workspace.id,
            })
            .collect(),
        workspaces: workspaces
            .into_iter()
            .map(|workspace| Workspace {
                id: workspace.id,
                name: workspace.name,
                monitor: workspace.monitor,
                windows: workspace.windows,
                urgent: workspace.urgent,
                fullscreen: workspace.has_fullscreen,
                last_window_title: workspace.last_window_title,
            })
            .collect(),
        error: None,
    })
}

pub async fn monitor(store: StateStore) {
    let client = HyprlandClient::default();
    loop {
        refresh(&client, &store).await;
        match client.event_socket().await {
            Ok(stream) => {
                let mut lines = BufReader::new(stream).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(event)) => {
                            if refresh_event(&event) {
                                refresh(&client, &store).await;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(%error, "Hyprland event stream failed");
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                tracing::debug!(%error, "Hyprland event socket unavailable");
            }
        }
        sleep(Duration::from_secs(1)).await;
    }
}

async fn refresh(client: &HyprlandClient, store: &StateStore) {
    match client.snapshot().await {
        Ok(snapshot) => store.update_workspaces(snapshot).await,
        Err(error) => {
            store
                .update_workspaces(WorkspaceState {
                    error: Some(error.to_string()),
                    ..WorkspaceState::default()
                })
                .await;
        }
    }
}

fn refresh_event(event: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "workspace",
        "focusedmon",
        "createworkspace",
        "destroyworkspace",
        "moveworkspace",
        "openwindow",
        "closewindow",
        "movewindow",
        "urgent",
        "fullscreen",
        "monitoradded",
        "monitorremoved",
        "configreloaded",
    ];
    PREFIXES.iter().any(|prefix| event.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{HyprlandClient, parse_snapshot, refresh_event};

    #[test]
    fn parses_and_orders_workspace_snapshot() {
        let state = parse_snapshot(
            r#"[{"id":5,"name":"5","monitor":"eDP-1","windows":1,"hasfullscreen":true,"lastwindowtitle":"Scratchpad"},{"id":1,"name":"1","monitor":"eDP-1","windows":2,"urgent":true}]"#,
            r#"[{"id":0,"name":"eDP-1","focused":true,"activeWorkspace":{"id":1}}]"#,
        ).unwrap();
        assert_eq!(state.focused_monitor.as_deref(), Some("eDP-1"));
        assert_eq!(
            state.workspaces.iter().map(|ws| ws.id).collect::<Vec<_>>(),
            [1, 5]
        );
        assert!(state.workspaces[0].urgent);
        assert!(state.workspaces[1].fullscreen);
    }

    #[test]
    fn filters_irrelevant_hyprland_events() {
        assert!(refresh_event("workspace>>2"));
        assert!(refresh_event("openwindow>>abc"));
        assert!(!refresh_event("activelayout>>keyboard,English"));
    }

    #[tokio::test]
    async fn reports_missing_instance() {
        let dir = tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("hypr"))
            .await
            .unwrap();
        let client = HyprlandClient::with_runtime(dir.path().to_path_buf(), None);
        assert!(client.instance_dir().await.is_err());
    }
}
