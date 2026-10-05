use std::{fs, os::unix::fs::symlink, time::Duration};

use tempfile::tempdir;
use tokio::{sync::broadcast, time::timeout};

use super::*;
use crate::state::DomainEvent;

fn candidate(directory: &Path, revision: &str) {
    let lane = directory.join("delayed");
    fs::create_dir_all(&lane).unwrap();
    fs::write(lane.join("ready-flake.lock"), "lock").unwrap();
    fs::write(lane.join("ready-revision"), revision).unwrap();
    fs::write(lane.join("ready-base-hash"), "def\n").unwrap();
    symlink("/nix/store/system", lane.join("system")).unwrap();
}

#[test]
fn requires_all_worker_readiness_files_and_ignores_obsolete_lanes() {
    let root = tempdir().unwrap();
    candidate(root.path(), "abc\n");
    let lane = root.path().join("delayed");
    // The last file in the worker's publication sequence is required too.
    assert!(!read_state(root.path()).unwrap().ready);
    fs::write(lane.join("ready-created-at"), "123").unwrap();
    let state = read_state(root.path()).unwrap();
    assert!(state.ready);
    assert_eq!(state.lanes[0].revision.as_deref(), Some("abc"));
    assert_eq!(state.lanes[0].created_at, Some(123));
    for name in [
        "ready-flake.lock",
        "ready-revision",
        "ready-base-hash",
        "ready-created-at",
        "system",
    ] {
        fs::rename(lane.join(name), lane.join("held")).unwrap();
        assert!(!read_state(root.path()).unwrap().ready, "missing {name}");
        fs::rename(lane.join("held"), lane.join(name)).unwrap();
    }
    fs::rename(&lane, root.path().join("fast")).unwrap();
    assert!(!read_state(root.path()).unwrap().ready);
}

async fn next_state(
    events: &mut broadcast::Receiver<DomainEvent>,
    matches: impl Fn(&UpdateState) -> bool,
) -> UpdateState {
    timeout(Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.unwrap();
            let state: UpdateState = serde_json::from_value(event.data).unwrap();
            if matches(&state) {
                return state;
            }
        }
    })
    .await
    .expect("update monitor did not publish the expected state")
}

#[tokio::test]
async fn monitor_follows_creation_replacement_removal_and_later_writes() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    let store = StateStore::default();
    let mut events = store.subscribe();
    let task = tokio::spawn(monitor_path(store, directory.clone()));
    next_state(&mut events, |s| !s.available).await;
    candidate(&directory, "first");
    fs::write(directory.join("delayed/ready-created-at"), "123").unwrap();
    next_state(&mut events, |s| s.ready).await;

    let replacement = root.path().join("replacement");
    candidate(&replacement, "second");
    fs::write(replacement.join("delayed/ready-created-at"), "456").unwrap();
    fs::rename(&directory, root.path().join("retired")).unwrap();
    fs::rename(&replacement, &directory).unwrap();
    next_state(&mut events, |s| {
        s.ready && s.lanes[0].revision.as_deref() == Some("second")
    })
    .await;
    // A refresh for the rename alone isn't enough: subsequent writes must
    // arrive through the new inode's watch, not the 60-second fallback.
    fs::write(directory.join("delayed/ready-revision"), "third").unwrap();
    next_state(&mut events, |s| {
        s.ready && s.lanes[0].revision.as_deref() == Some("third")
    })
    .await;

    fs::remove_dir_all(&directory).unwrap();
    next_state(&mut events, |s| !s.available).await;
    candidate(&directory, "fourth");
    next_state(&mut events, |s| s.available && !s.ready).await;
    fs::write(directory.join("delayed/ready-created-at"), "789").unwrap();
    next_state(&mut events, |s| s.ready).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
