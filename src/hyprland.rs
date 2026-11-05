use std::{future::Future, time::Duration};

use anyhow::Result;
use shelllist_hyprland::Event;
use tokio::{
    sync::{mpsc, watch},
    time::sleep,
};

use crate::{
    model::{ActiveWindow, MonitorState, Workspace, WorkspaceState},
    state::StateStore,
};

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

    pub(crate) async fn execute(&self, command: &shelllist_hyprland::Lua) -> Result<()> {
        shelllist_hyprland::execute(&self.ipc, command).await
    }
    pub(crate) async fn options(&self, names: &[&str]) -> Result<String> {
        self.ipc.options(names).await
    }

    pub(crate) async fn snapshot(&self) -> Result<WorkspaceState> {
        Ok(project_status(self.ipc.status().await?))
    }

    pub(crate) async fn focus_workspace(
        &self,
        workspace_id: i64,
        on_current_monitor: bool,
    ) -> Result<()> {
        self.execute(&shelllist_hyprland::lua::focus_workspace(
            workspace_id,
            on_current_monitor,
        )?)
        .await
    }
}

#[cfg(test)]
fn parse_snapshot(workspaces: &str, monitors: &str, active: &str) -> Result<WorkspaceState> {
    Ok(project_status(
        shelllist_hyprland::workspace::Status::parse(&format!(
            "{workspaces}\n\n{monitors}\n\n{active}"
        ))?,
    ))
}
fn project_status(status: shelllist_hyprland::workspace::Status) -> WorkspaceState {
    let shelllist_hyprland::workspace::Status {
        mut workspaces,
        mut monitors,
        active_window,
    } = status;
    workspaces.sort_by_key(|workspace| workspace.id);
    monitors.sort_by_key(|monitor| monitor.id);
    let focused_monitor = monitors
        .iter()
        .find(|monitor| monitor.focused)
        .map(|monitor| monitor.name.clone());
    let active_window = active_window.map(|window| ActiveWindow {
        address: window.address,
        title: window.title,
        class_name: window.class,
        initial_class: window.initial_class,
        workspace_id: window.workspace.id,
        workspace_address: window.workspace.address,
        fullscreen: window.fullscreen != 0,
        floating: window.floating,
    });
    WorkspaceState {
        lifecycle: Default::default(),
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
                active_workspace_address: monitor.active_workspace.address,
            })
            .collect(),
        workspaces: workspaces
            .into_iter()
            // Publish named regular workspaces without giving them a numeric
            // lifecycle identity. The numbered strip filters these explicitly.
            .filter(|workspace| !workspace.special)
            .map(|workspace| Workspace {
                id: workspace.id,
                address: workspace.identity.address,
                name: workspace.identity.name,
                monitor: workspace.monitor,
                windows: workspace.windows,
                urgent: false,
                fullscreen: workspace.has_fullscreen,
                last_window_title: workspace.last_window_title,
            })
            .collect(),
        error: None,
    }
}

pub(crate) async fn monitor(store: StateStore) {
    let client = HyprlandClient::default();
    let lifecycle = store.workspace_lifecycle.clone();
    let (sender, events) = mpsc::channel(64);
    // Both futures are owned by this monitor; shutdown drops the event receiver
    // and cancels pending reads without leaving an orphan watcher task.
    tokio::join!(
        client.ipc.clone().watch_events(sender),
        monitor_with(store, events, || async {
            let mut state = client.snapshot().await?;
            lifecycle.project(&mut state).await;
            Ok(state)
        }),
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
            store.workspace_lifecycle.handoff_event(&event);
            let refresh = match event {
                Event::Connected { .. } => {
                    connected = true;
                    store.set_hyprland_connected(connected);
                    true
                }
                Event::Disconnected(cause) => {
                    connected = false;
                    store.set_hyprland_connected(connected);
                    if let Some(cause) = cause {
                        tracing::warn!(%cause, "Hyprland event stream failed; continuing reconciliation");
                    }
                    true
                }
                Event::ConnectFailed(cause) => {
                    connected = false;
                    store.set_hyprland_connected(connected);
                    tracing::warn!(%cause, "Hyprland event connection failed; continuing reconciliation");
                    true
                }
                Event::SessionGone { configured, live } => {
                    store.compositor_failed(shelllist_hyprland::IpcError::SessionGone {
                        configured,
                        live,
                    });
                    return;
                }
                Event::Lagged => {
                    store.compositor_changed.notify_one();
                    store.work_area_changed.notify_one();
                    true
                }
                Event::Compositor(event) => {
                    if event.invalidates_preferences() {
                        store.compositor_changed.notify_one();
                    }
                    if event.invalidates_geometry() {
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
            _ = store.workspace_lifecycle.changed.notified() => {},
            changed = updates.changed() => {
                if changed.is_err() { return; }
                // A fixed window (not reset by new events) bounds burst latency.
                sleep(Duration::from_millis(75)).await;
            }
            _ = sleep(Duration::from_secs(1)), if retry => {},
        }
    }
}

fn refresh_event(event: &shelllist_hyprland::CompositorEvent) -> bool {
    use shelllist_hyprland::CompositorEvent::*;
    matches!(
        event,
        WorkspaceChanged
            | FocusedMonitorChanged
            | ActiveWindowChanged
            | WindowTitleChanged
            | WorkspaceCreated
            | WorkspaceDestroyed
            | WorkspaceMoved
            | WindowOpened
            | WindowClosed
            | WindowMoved { .. }
            | WindowUrgent
            | FullscreenChanged
            | MonitorAdded
            | MonitorRemoved
            | ConfigReloaded
            | WorkspaceIdChanged
            | Malformed { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::{monitor_with, parse_snapshot};
    use crate::state::StateStore;
    use shelllist_hyprland::Event;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::{Semaphore, mpsc};

    #[tokio::test]
    async fn session_loss_is_retained_for_the_daemon_cleanup_path() {
        let store = StateStore::default();
        let (sender, events) = mpsc::channel(1);
        sender
            .send(Event::SessionGone {
                configured: "old".into(),
                live: "new".into(),
            })
            .await
            .unwrap();
        monitor_with(store.clone(), events, || async { Ok(Default::default()) }).await;
        let error = store.compositor_failure().await;
        assert!(
            matches!(error.downcast_ref::<shelllist_hyprland::IpcError>(), Some(shelllist_hyprland::IpcError::SessionGone { configured, live }) if configured=="old" && live=="new")
        );
    }

    #[test]
    fn special_workspace_without_number_does_not_hide_numbered_workspaces() {
        let state = parse_snapshot(
            r#"[{"id":1,"name":"1","monitor":"eDP-1","windows":0},{"address":"special:probe","type":"special","name":"special:probe","monitor":"eDP-1","windows":1}]"#,
            r#"[{"id":0,"name":"eDP-1","focused":true,"activeWorkspace":{"id":1}}]"#,
            r#"{"address":"0x123","pid":42,"title":"Special window","workspace":{"address":"special:probe","name":"special:probe","type":"special"}}"#,
        ).unwrap();
        assert!(state.available);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].id, Some(1));
        assert_eq!(state.active_window.unwrap().title, "Special window");
    }

    #[test]
    fn named_workspace_capture_is_published_without_a_synthetic_id() {
        let capture: serde_json::Value = serde_json::from_str(include_str!(
            "../../daemon-framework/crates/shelllist-hyprland/tests/fixtures/0.57.1/named.json"
        ))
        .unwrap();
        let state = parse_snapshot(
            &capture["workspaces"].to_string(),
            &capture["monitors"].to_string(),
            &capture["activewindow"].to_string(),
        )
        .unwrap();
        let named = state
            .workspaces
            .iter()
            .find(|w| w.address.as_deref() == Some("ipc-review"))
            .unwrap();
        assert_eq!(named.id, None);
        let monitor = state.monitors.iter().find(|m| m.focused).unwrap();
        assert_eq!(monitor.active_workspace_id, None);
        assert_eq!(
            monitor.active_workspace_address.as_deref(),
            Some("ipc-review")
        );
        assert_eq!(state.active_window.as_ref().unwrap().workspace_id, None);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_snapshots_do_not_block_invalidations_and_bursts_coalesce() {
        let store = StateStore::default();
        let (events, receiver) = mpsc::channel(128);
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let gate = Arc::new(Semaphore::new(0));
        let (count, start, blocked) = (calls.clone(), started.clone(), gate.clone());
        events
            .send(Event::Connected {
                instance: "test".into(),
            })
            .await
            .unwrap();
        let task = tokio::spawn(monitor_with(store.clone(), receiver, move || {
            let call = count.fetch_add(1, Ordering::SeqCst);
            let (start, blocked) = (start.clone(), blocked.clone());
            async move {
                start.add_permits(1);
                if call == 0 {
                    blocked.acquire().await.unwrap().forget();
                }
                parse_snapshot(
                    r#"[{"id":5,"name":"5","monitor":"eDP-1","windows":1,"hasfullscreen":true},{"id":1,"name":"1","monitor":"eDP-1","windows":2}]"#,
                    r#"[{"id":0,"name":"eDP-1","focused":true,"activeWorkspace":{"id":1}}]"#,
                    r#"{"address":"0x123","pid":42,"title":"Terminal","class":"ghostty","workspace":{"id":1},"fullscreen":0,"floating":false}"#,
                )
            }
        }));
        started.acquire().await.unwrap().forget();
        store.compositor_changed.notified().await;
        store.work_area_changed.notified().await;
        for _ in 0..100 {
            events
                .send(Event::Compositor(
                    shelllist_hyprland::CompositorEvent::parse("workspace>>1"),
                ))
                .await
                .unwrap();
        }
        events
            .send(Event::Compositor(
                shelllist_hyprland::CompositorEvent::ConfigReloaded,
            ))
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
        let snapshot = store.snapshot().await.workspaces;
        assert!(snapshot.available);
        assert_eq!(snapshot.focused_monitor.as_deref(), Some("eDP-1"));
        assert_eq!(
            snapshot
                .workspaces
                .iter()
                .map(|ws| ws.id)
                .collect::<Vec<_>>(),
            [Some(1), Some(5)]
        );
        assert!(!snapshot.workspaces[0].urgent);
        assert!(snapshot.workspaces[1].fullscreen);
        assert_eq!(snapshot.active_window.unwrap().title, "Terminal");
        for failure in [
            Event::Disconnected(None),
            Event::ConnectFailed("connection refused".into()),
            Event::Disconnected(Some("invalid UTF-8".into())),
        ] {
            events.send(failure).await.unwrap();
            store.compositor_changed.notified().await;
            store.work_area_changed.notified().await;
            started.acquire().await.unwrap().forget();
            assert!(!store.hyprland_connected());
        }
        events
            .send(Event::Connected {
                instance: "test".into(),
            })
            .await
            .unwrap();
        store.compositor_changed.notified().await;
        store.work_area_changed.notified().await;
        started.acquire().await.unwrap().forget();
        assert!(store.hyprland_connected());
        let recovered_calls = calls.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), recovered_calls);
        drop(events);
        task.await.unwrap();
    }
}
