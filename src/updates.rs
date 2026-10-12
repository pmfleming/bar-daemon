use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use shelllist_update_status::{FILE_NAME, MAX_BYTES, Readiness};

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
    let (lane, error) = match read_lane(directory, "delayed") {
        Ok(lane) => (lane, None),
        Err(error) => (
            UpdateLane {
                name: "delayed".into(),
                ..Default::default()
            },
            Some(format!("Update readiness is unavailable: {error:#}")),
        ),
    };
    Ok(UpdateState {
        available: true,
        ready: lane.ready,
        lanes: vec![lane],
        jobs: jobs::read(directory),
        state_directory: directory_display(directory),
        error,
    })
}

fn read_lane(directory: &Path, name: &str) -> Result<UpdateLane> {
    let bytes = shelllist_daemon_core::read_bytes_bounded(
        &directory.join(name).join(FILE_NAME),
        MAX_BYTES as u64,
    )?
    .context("readiness.json is missing; update-worker must publish its public status")?;
    let record = Readiness::decode(&bytes)?;
    let Some(candidate) = record.candidate else {
        return Ok(UpdateLane {
            name: name.into(),
            ..Default::default()
        });
    };
    // Check the path contained in this record, never the mutable legacy link.
    ensure!(
        std::fs::metadata(&candidate.system)?.is_dir(),
        "Candidate system is not a directory"
    );
    Ok(UpdateLane {
        name: name.into(),
        ready: true,
        revision: Some(candidate.revision),
        base_hash: Some(candidate.base_hash),
        created_at: Some(candidate.created_at),
        auto_apply: candidate.auto_apply,
        system: Some(directory_display(&candidate.system)),
    })
}

fn directory_display(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests;
