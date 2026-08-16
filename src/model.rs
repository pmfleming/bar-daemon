use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BarSnapshot {
    pub workspaces: WorkspaceState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub available: bool,
    pub focused_monitor: Option<String>,
    pub monitors: Vec<MonitorState>,
    pub workspaces: Vec<Workspace>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorState {
    pub id: i64,
    pub name: String,
    pub focused: bool,
    pub active_workspace_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: u32,
    pub urgent: bool,
    pub fullscreen: bool,
    pub last_window_title: String,
}
