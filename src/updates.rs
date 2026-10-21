use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use shelllist_update_status::{FILE_NAME, MAX_BYTES, Readiness};

use crate::{
    model::{UpdateLane, UpdateState},
    state::StateStore,
};

mod catalog;
mod desktop;
#[cfg(test)]
mod desktop_tests;
mod files;
pub(crate) mod history;
mod jobs;
mod json;
mod manual;
#[cfg(test)]
mod manual_tests;
pub(crate) mod overview;
mod tray;
mod watch;

pub(crate) async fn open(run_id: &str, token: Option<&str>) -> Result<()> {
    ensure!(
        run_id.is_empty() || shelllist_update_status::valid_run_id(run_id),
        "Invalid update run identity"
    );
    let mut command = tokio::process::Command::new(
        std::env::var_os("BAR_DAEMON_SHELLLIST").unwrap_or_else(|| "shelllist".into()),
    );
    if run_id.is_empty() {
        command.args(["updates", "open"]);
    } else {
        command.args(["updates", "run", run_id]);
    }
    if let Some(token) = token.filter(|token| token.len() <= 4096) {
        command.env("XDG_ACTIVATION_TOKEN", token);
    }
    command.kill_on_drop(true);
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), command.status())
        .await
        .context("Updates launcher timed out")??;
    ensure!(status.success(), "Updates launcher failed");
    Ok(())
}

const DEFAULT_STATE_DIR: &str = "/var/lib/nixos-delayed-updates-v2";

pub(crate) fn state_dir() -> PathBuf {
    std::env::var_os("BAR_DAEMON_UPDATE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

pub(crate) async fn monitor(
    store: StateStore,
    service: std::sync::Arc<crate::activity::notifications::service::NotificationService>,
) {
    monitor_path(store, state_dir(), Some(service)).await;
}

async fn monitor_path(
    store: StateStore,
    directory: PathBuf,
    service: Option<std::sync::Arc<crate::activity::notifications::service::NotificationService>>,
) {
    use ksni::TrayMethods;
    let enabled =
        service.is_some() && std::env::var("BAR_DAEMON_UPDATE_DESKTOP").as_deref() != Ok("0");
    let collect_history = service.is_some();
    let manual_directory = collect_history.then(manual::directory).flatten();
    let mut publisher = service.filter(|_| enabled).map(desktop::Publisher::new);
    let tray = if enabled {
        tray::Tray
            .assume_sni_available(true)
            .spawn()
            .await
            .map_err(|error| tracing::warn!(%error, "Update tray unavailable"))
            .ok()
    } else {
        None
    };
    // The library owns watcher loss/re-registration. Cancellation removes the item.
    struct TrayGuard(Option<ksni::Handle<tray::Tray>>);
    impl Drop for TrayGuard {
        fn drop(&mut self) {
            if let Some(handle) = &self.0 {
                handle.shutdown();
            }
        }
    }
    let _tray = TrayGuard(tray);
    let mut watch = watch::Watch::new();
    let mut manual_watch = watch::Watch::new_rebuilds();
    let mut last_catalog = None;
    loop {
        watch.refresh(&directory);
        if let Some(path) = &manual_directory {
            manual_watch.refresh(path);
        }
        refresh_path(&store, directory.clone()).await;
        if collect_history {
            let path = directory.clone();
            let manual = manual_directory.clone();
            match tokio::task::spawn_blocking(move || {
                history::load_catalog(&path, manual.as_deref())
            })
            .await
            {
                Ok(catalog) => {
                    match catalog.revision() {
                        Ok(revision) if Some(revision) != last_catalog => {
                            last_catalog = Some(revision);
                            store.invalidate_update_history().await;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "Update history fingerprint unavailable")
                        }
                        _ => {}
                    }
                    if let Some(publisher) = &mut publisher
                        && let Err(error) = publisher.reconcile(&catalog).await
                    {
                        tracing::warn!(%error, "Update outcome delivery unavailable");
                    }
                }
                Err(error) => tracing::warn!(%error, "Update history reader stopped"),
            }
        }
        tokio::select! { _ = watch.wait() => {}, _ = manual_watch.wait(), if manual_directory.is_some() => {} }
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
mod producer_tests;
#[cfg(test)]
mod tests;
