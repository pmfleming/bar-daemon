use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{
    model::{UpdateLane, UpdateState},
    state::StateStore,
};

mod jobs;
mod watch;

const DEFAULT_STATE_DIR: &str = "/var/lib/nixos-delayed-updates-v2";

pub(crate) fn state_dir() -> PathBuf {
    std::env::var_os("BAR_DAEMON_UPDATE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

pub(crate) async fn monitor(store: StateStore) {
    monitor_path(store, state_dir()).await;
}

async fn monitor_path(store: StateStore, directory: PathBuf) {
    let mut watch = watch::Watch::new();
    loop {
        watch.refresh(&directory);
        refresh_path(&store, directory.clone()).await;
        watch.wait().await;
    }
}

pub(crate) async fn refresh_default(store: &StateStore) -> Result<UpdateState> {
    let directory = state_dir();
    let state = tokio::task::spawn_blocking(move || read_state(&directory))
        .await
        .context("join update status refresh")??;
    store.update_updates(state.clone()).await;
    Ok(state)
}

async fn refresh_path(store: &StateStore, directory: PathBuf) {
    let state_directory = directory_display(&directory);
    let result = tokio::task::spawn_blocking(move || read_state(&directory))
        .await
        .unwrap_or_else(|error| Err(error.into()));
    store
        .update_updates(result.unwrap_or_else(|error| UpdateState {
            state_directory,
            error: Some(error.to_string()),
            ..UpdateState::default()
        }))
        .await;
}

fn read_state(directory: &Path) -> Result<UpdateState> {
    if !directory.exists() {
        return Ok(UpdateState {
            available: false,
            state_directory: directory_display(directory),
            error: None,
            ..UpdateState::default()
        });
    }
    let lanes = ["delayed"]
        .into_iter()
        .map(|name| read_lane(directory, name))
        .collect::<Vec<_>>();
    Ok(UpdateState {
        available: true,
        ready: lanes.iter().any(|lane| lane.ready),
        lanes,
        jobs: jobs::read(directory),
        state_directory: directory_display(directory),
        error: None,
    })
}

fn read_lane(directory: &Path, name: &str) -> UpdateLane {
    let lane = directory.join(name);
    // Keep in sync with update-daemon's ready_is_complete predicate.
    let required = [
        "ready-flake.lock",
        "ready-revision",
        "ready-base-hash",
        "ready-created-at",
    ];
    let ready =
        required.iter().all(|file| lane.join(file).is_file()) && lane.join("system").is_symlink();
    UpdateLane {
        name: name.into(),
        ready,
        revision: read_trimmed(&lane.join("ready-revision")),
        base_hash: read_trimmed(&lane.join("ready-base-hash")),
        created_at: read_trimmed(&lane.join("ready-created-at"))
            .and_then(|value| value.parse().ok()),
        auto_apply: lane.join("auto-apply").exists(),
        system: std::fs::read_link(lane.join("system"))
            .ok()
            .map(|path| directory_display(&path)),
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn directory_display(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests;
