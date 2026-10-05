use std::{future::Future, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use shelllist_hyprland::Event;
use tokio::{
    sync::{mpsc, watch},
    time::sleep,
};

use crate::{
    model::{ActiveWindow, MonitorState, Workspace, WorkspaceState},
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

#[derive(Debug, Default, Deserialize)]
struct HyprActiveWindow {
    #[serde(default)]
    address: String,
    #[serde(default)]
    title: String,
    #[serde(default, rename = "class")]
    class_name: String,
    #[serde(default, rename = "initialClass")]
    initial_class: String,
    #[serde(default)]
    workspace: HyprActiveWorkspace,
    #[serde(default)]
    fullscreen: i8,
    #[serde(default)]
    floating: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct HyprlandClient {
    ipc: shelllist_hyprland::Client,
}

impl Default for HyprlandClient {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl HyprlandClient {
    pub(crate) fn from_environment() -> Self {
        Self {
            ipc: shelllist_hyprland::Client::from_environment(),
        }
    }

    pub(crate) async fn request(&self, command: &str) -> Result<String> {
        self.ipc.request(command).await
    }

    pub(crate) async fn snapshot(&self) -> Result<WorkspaceState> {
        let (workspaces, monitors, active_window) = tokio::try_join!(
            self.request("j/workspaces"),
            self.request("j/monitors"),
            self.request("j/activewindow")
        )?;
        parse_snapshot(&workspaces, &monitors, &active_window)
    }

    pub(crate) async fn focus_workspace(
        &self,
        workspace_id: i64,
        on_current_monitor: bool,
    ) -> Result<()> {
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
}

fn parse_snapshot(
    workspaces_json: &str,
    monitors_json: &str,
    active_window_json: &str,
) -> Result<WorkspaceState> {
    let mut workspaces: Vec<HyprWorkspace> =
        serde_json::from_str(workspaces_json).context("decode Hyprland workspaces")?;
    let mut monitors: Vec<HyprMonitor> =
        serde_json::from_str(monitors_json).context("decode Hyprland monitors")?;
    let active_window: HyprActiveWindow =
        serde_json::from_str(active_window_json).context("decode Hyprland active window")?;
    workspaces.sort_by_key(|workspace| workspace.id);
    monitors.sort_by_key(|monitor| monitor.id);
    let focused_monitor = monitors
        .iter()
        .find(|monitor| monitor.focused)
        .map(|monitor| monitor.name.clone());
    let active_window = if active_window.address.is_empty()
        && active_window.title.is_empty()
        && active_window.class_name.is_empty()
    {
        None
    } else {
        Some(ActiveWindow {
            address: active_window.address,
            title: active_window.title,
            class_name: active_window.class_name,
            initial_class: active_window.initial_class,
            workspace_id: active_window.workspace.id,
            fullscreen: active_window.fullscreen != 0,
            floating: active_window.floating,
        })
    };
    Ok(WorkspaceState {
        available: true,
        focused_monitor,
        active_window,
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

pub(crate) async fn monitor(store: StateStore) {
    let client = HyprlandClient::default();
    let (sender, events) = mpsc::channel(64);
    // Both futures are owned by this monitor; shutdown drops the event receiver
    // and cancels pending reads without leaving an orphan watcher task.
    tokio::join!(
        shelllist_hyprland::watch_events_detailed(sender),
        monitor_with(store, events, || client.snapshot()),
    );
}

async fn monitor_with<F, Fut>(store: StateStore, mut events: mpsc::Receiver<Event>, fetch: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<WorkspaceState>>,
{
    let (changes, updates) = watch::channel(false);
    // Intake must not await a snapshot: geometry/preference invalidations keep
    // flowing even during slow IPC. Watch coalesces workspace refresh requests.
    let intake = async {
        let mut connected = false;
        while let Some(event) = events.recv().await {
            let refresh = match event {
                Event::Connected | Event::Disconnected => {
                    connected = event == Event::Connected;
                    store.set_hyprland_connected(connected);
                    true
                }
                Event::Message(event) => {
                    if shelllist_hyprland::preferences::preference_event(&event) {
                        store.compositor_changed.notify_one();
                    }
                    if shelllist_hyprland::work_area::geometry_event(&event) {
                        store.work_area_changed.notify_one();
                    }
                    refresh_event(&event)
                }
            };
            if refresh {
                changes.send_replace(connected);
            }
        }
    };
    tokio::select! {
        _ = intake => {},
        _ = refresh_workspaces(&store, updates, fetch) => {},
    }
}

async fn refresh_workspaces<F, Fut>(
    store: &StateStore,
    mut updates: watch::Receiver<bool>,
    fetch: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<WorkspaceState>>,
{
    loop {
        let connected = *updates.borrow_and_update();
        let state = fetch().await.unwrap_or_else(|error| WorkspaceState {
            error: Some(error.to_string()),
            ..WorkspaceState::default()
        });
        let retry = !connected || !state.available;
        store.update_workspaces(state).await;
        tokio::select! {
            changed = updates.changed() => {
                if changed.is_err() { return; }
                // A fixed window (not reset by new events) bounds burst latency.
                sleep(Duration::from_millis(75)).await;
            }
            _ = sleep(Duration::from_secs(1)), if retry => {},
        }
    }
}

fn refresh_event(event: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "workspace",
        "focusedmon",
        "activewindow",
        "windowtitle",
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
    use super::{monitor_with, parse_snapshot, refresh_workspaces};
    use crate::{model::WorkspaceState, state::StateStore};
    use shelllist_hyprland::Event;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::{Semaphore, mpsc, watch};

    #[tokio::test(start_paused = true)]
    async fn slow_snapshots_do_not_block_invalidations_and_bursts_coalesce() {
        let store = StateStore::default();
        let (events, receiver) = mpsc::channel(128);
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let gate = Arc::new(Semaphore::new(0));
        let (count, start, blocked) = (calls.clone(), started.clone(), gate.clone());
        events.send(Event::Connected).await.unwrap();
        let task = tokio::spawn(monitor_with(store.clone(), receiver, move || {
            let call = count.fetch_add(1, Ordering::SeqCst);
            let (start, blocked) = (start.clone(), blocked.clone());
            async move {
                start.add_permits(1);
                if call == 0 {
                    blocked.acquire().await.unwrap().forget();
                }
                Ok(WorkspaceState {
                    available: true,
                    ..Default::default()
                })
            }
        }));
        started.acquire().await.unwrap().forget();
        store.compositor_changed.notified().await;
        store.work_area_changed.notified().await;
        for _ in 0..100 {
            events
                .send(Event::Message("workspace>>1".into()))
                .await
                .unwrap();
        }
        events
            .send(Event::Message("configreloaded>>".into()))
            .await
            .unwrap();
        store.compositor_changed.notified().await; // delivered while first fetch is blocked
        store.work_area_changed.notified().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(store.hyprland_connected());
        gate.add_permits(1);
        started.acquire().await.unwrap().forget();
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(store.snapshot().await.workspaces.available);
        events.send(Event::Disconnected).await.unwrap();
        store.compositor_changed.notified().await;
        store.work_area_changed.notified().await;
        started.acquire().await.unwrap().forget();
        assert!(!store.hyprland_connected());
        drop(events);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn workspace_failures_retry_without_new_events_but_success_stops_polling() {
        let store = StateStore::default();
        let (changes, updates) = watch::channel(true);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            refresh_workspaces(&store, updates, move || {
                let call = count.fetch_add(1, Ordering::SeqCst);
                async move {
                    if call == 0 {
                        anyhow::bail!("offline");
                    }
                    Ok(WorkspaceState {
                        available: true,
                        ..Default::default()
                    })
                }
            })
            .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        drop(changes);
        task.await.unwrap();
    }

    #[test]
    fn parses_and_orders_workspace_snapshot() {
        let state = parse_snapshot(
            r#"[{"id":5,"name":"5","monitor":"eDP-1","windows":1,"hasfullscreen":true,"lastwindowtitle":"Scratchpad"},{"id":1,"name":"1","monitor":"eDP-1","windows":2,"urgent":true}]"#,
            r#"[{"id":0,"name":"eDP-1","focused":true,"activeWorkspace":{"id":1}}]"#,
            r#"{"address":"0x123","title":"Terminal","class":"com.mitchellh.ghostty","initialClass":"ghostty","workspace":{"id":1},"fullscreen":0,"floating":false}"#,
        ).unwrap();
        assert_eq!(state.focused_monitor.as_deref(), Some("eDP-1"));
        assert_eq!(
            state.workspaces.iter().map(|ws| ws.id).collect::<Vec<_>>(),
            [1, 5]
        );
        assert!(state.workspaces[0].urgent);
        assert!(state.workspaces[1].fullscreen);
        let active_window = state.active_window.unwrap();
        assert_eq!(active_window.title, "Terminal");
        assert_eq!(active_window.class_name, "com.mitchellh.ghostty");
        assert_eq!(active_window.workspace_id, 1);
    }
}
