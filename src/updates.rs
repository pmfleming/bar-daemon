use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use tokio::{sync::mpsc, time::sleep};

use crate::{
    model::{UpdateLane, UpdateState},
    state::StateStore,
};

const DEFAULT_STATE_DIR: &str = "/var/lib/nixos-delayed-updates-v2";

pub fn state_dir() -> PathBuf {
    std::env::var_os("BAR_DAEMON_UPDATE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

pub async fn monitor(store: StateStore) {
    let directory = state_dir();
    let (tx, mut rx) = mpsc::channel(16);
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if result.is_ok() {
            let _ = tx.try_send(());
        }
    })
    .ok();
    let mut watched_path = None;
    loop {
        if let Some(watcher) = watcher.as_mut() {
            update_watch(watcher, &directory, &mut watched_path);
        }
        refresh_path(&store, directory.clone()).await;
        tokio::select! {
            value = rx.recv() => if value.is_none() { sleep(Duration::from_secs(2)).await; },
            _ = sleep(Duration::from_secs(60)) => {}
        }
    }
}

fn update_watch(watcher: &mut impl Watcher, directory: &Path, watched_path: &mut Option<PathBuf>) {
    let Some((next_path, mode)) = watch_target(directory) else {
        return;
    };
    if watched_path.as_ref() == Some(&next_path) {
        return;
    }
    if let Some(previous) = watched_path.take()
        && let Err(error) = watcher.unwatch(&previous)
    {
        tracing::debug!(%error, path = %previous.display(), "could not replace NixOS update watch");
    }
    match watcher.watch(&next_path, mode) {
        Ok(()) => *watched_path = Some(next_path),
        Err(error) => {
            tracing::debug!(%error, path = %next_path.display(), "NixOS update watcher unavailable")
        }
    }
}

fn watch_target(directory: &Path) -> Option<(PathBuf, RecursiveMode)> {
    if directory.is_dir() {
        return Some((directory.to_path_buf(), RecursiveMode::Recursive));
    }
    directory
        .ancestors()
        .skip(1)
        .find(|path| path.is_dir())
        .map(|path| (path.to_path_buf(), RecursiveMode::NonRecursive))
}

pub async fn refresh_default(store: &StateStore) -> Result<UpdateState> {
    let directory = state_dir();
    let state = tokio::task::spawn_blocking(move || read_state(&directory))
        .await
        .context("join update status refresh")??;
    store.update_updates(state.clone()).await;
    Ok(state)
}

async fn refresh_path(store: &StateStore, directory: PathBuf) {
    match tokio::task::spawn_blocking(move || read_state(&directory)).await {
        Ok(Ok(state)) => store.update_updates(state).await,
        Ok(Err(error)) => {
            store
                .update_updates(UpdateState {
                    state_directory: directory_display(&state_dir()),
                    error: Some(error.to_string()),
                    ..UpdateState::default()
                })
                .await
        }
        Err(error) => {
            store
                .update_updates(UpdateState {
                    state_directory: directory_display(&state_dir()),
                    error: Some(error.to_string()),
                    ..UpdateState::default()
                })
                .await
        }
    }
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
    let lanes = ["fast", "delayed"]
        .into_iter()
        .map(|name| read_lane(directory, name))
        .collect::<Vec<_>>();
    Ok(UpdateState {
        available: true,
        ready: lanes.iter().any(|lane| lane.ready),
        lanes,
        state_directory: directory_display(directory),
        error: None,
    })
}

fn read_lane(directory: &Path, name: &str) -> UpdateLane {
    let lane = directory.join(name);
    let required = ["ready-flake.lock", "ready-revision", "ready-base-hash"];
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
mod tests {
    use std::{fs, os::unix::fs::symlink};
    use tempfile::tempdir;

    use super::{read_state, watch_target};

    #[test]
    fn requires_complete_ready_lane() {
        let root = tempdir().unwrap();
        let fast = root.path().join("fast");
        fs::create_dir(&fast).unwrap();
        fs::write(fast.join("ready-flake.lock"), "lock").unwrap();
        assert!(!read_state(root.path()).unwrap().ready);
        fs::write(fast.join("ready-revision"), "abc\n").unwrap();
        fs::write(fast.join("ready-base-hash"), "def\n").unwrap();
        fs::write(fast.join("ready-created-at"), "123").unwrap();
        symlink("/nix/store/system", fast.join("system")).unwrap();
        let state = read_state(root.path()).unwrap();
        assert!(state.ready);
        assert_eq!(state.lanes[0].revision.as_deref(), Some("abc"));
        assert_eq!(state.lanes[0].created_at, Some(123));
    }

    #[test]
    fn absent_state_directory_is_not_an_error() {
        let root = tempdir().unwrap();
        let state = read_state(&root.path().join("missing")).unwrap();
        assert!(!state.available);
        assert!(state.error.is_none());
    }

    #[test]
    fn watches_parent_until_state_directory_exists() {
        let root = tempdir().unwrap();
        let directory = root.path().join("updates");
        assert_eq!(watch_target(&directory).unwrap().0, root.path());
        fs::create_dir(&directory).unwrap();
        assert_eq!(watch_target(&directory).unwrap().0, directory);
    }
}
