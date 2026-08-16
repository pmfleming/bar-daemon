use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BarSnapshot {
    pub workspaces: WorkspaceState,
    pub media: MediaState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaState {
    pub available: bool,
    pub active_player: Option<String>,
    pub players: Vec<MediaPlayer>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaPlayer {
    pub id: String,
    pub identity: String,
    pub desktop_entry: String,
    pub playback_status: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub can_control: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
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
