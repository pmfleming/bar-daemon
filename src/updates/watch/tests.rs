use super::*;
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    time::Instant,
};

fn watches_inode(path: &Path) -> bool {
    let needle = format!(" ino:{:x} ", path.metadata().unwrap().ino());
    fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .flatten()
        .any(|entry| {
            fs::read_to_string(entry.path())
                .unwrap_or_default()
                .lines()
                .any(|line| line.starts_with("inotify wd:") && line.contains(&needle))
        })
}

#[test]
fn manual_ledger_watch_does_not_descend_into_build_caches() {
    let ledger = tempfile::tempdir().unwrap();
    let closure = tempfile::tempdir().unwrap();
    let cache = ledger.path().join("check-cache");
    fs::create_dir(&cache).unwrap();
    symlink(closure.path(), cache.join("checks")).unwrap();
    let mut watch = Watch::new_rebuilds();
    watch.refresh(ledger.path());
    assert!(watches_inode(ledger.path()));
    assert!(
        !watches_inode(&cache),
        "flat ledger must not watch build-cache subtrees"
    );
    assert!(
        !watches_inode(closure.path()),
        "must not traverse Nix closure symlinks"
    );
}

#[test]
fn worker_watch_keeps_real_subdirectories_but_not_symlink_targets() {
    let state = tempfile::tempdir().unwrap();
    let closure = tempfile::tempdir().unwrap();
    let history = state.path().join("history");
    fs::create_dir(&history).unwrap();
    symlink(closure.path(), state.path().join("candidate-system")).unwrap();
    let mut watch = Watch::new();
    watch.refresh(state.path());
    assert!(watches_inode(&history));
    assert!(!watches_inode(closure.path()));
}

#[test]
fn rebuild_storms_back_off_and_watch_budget_falls_back_to_polling() {
    let state = tempfile::tempdir().unwrap();
    let mut watch = Watch::new();
    let mut now = Instant::now();
    watch.refresh_at(state.path(), now);
    for delay in [1, 2, 4, 8, 16, 30, 30] {
        assert!(watch.watcher.is_some());
        watch.rebuild.store(true, Ordering::Relaxed);
        watch.refresh_at(state.path(), now);
        assert!(watch.watcher.is_none());
        assert_eq!(watch.recovery.deadline() - now, Duration::from_secs(delay));
        watch.rebuild.store(true, Ordering::Relaxed);
        watch.refresh_at(state.path(), now);
        assert_eq!(watch.recovery.deadline() - now, Duration::from_secs(delay));
        now = watch.recovery.deadline();
        watch.refresh_at(state.path(), now);
    }
    drop(watch);
    for i in 0..DEFAULT_WATCH_LIMIT {
        fs::create_dir(state.path().join(i.to_string())).unwrap();
    }
    let mut watch = Watch::new();
    watch.refresh(state.path());
    assert!(watch.watcher.is_none());
    assert!(
        !watches_inode(state.path()),
        "budget failure must not leave partial watches"
    );
}

#[tokio::test]
async fn new_subdirectories_are_registered_after_bounded_recovery() {
    let state = tempfile::tempdir().unwrap();
    let mut watch = Watch::new();
    watch.refresh(state.path());
    let child = state.path().join("new-history");
    fs::create_dir(&child).unwrap();
    tokio::time::timeout(Duration::from_secs(2), watch.wait())
        .await
        .unwrap();
    watch.refresh(state.path());
    assert!(watch.watcher.is_none());
    watch.refresh_at(state.path(), watch.recovery.deadline());
    assert!(watches_inode(&child));
}

/// Reproducible registration work, not a real-window responsiveness benchmark.
#[test]
#[ignore = "manual performance probe; run alone with --ignored --nocapture"]
fn profile_manual_watch_registration() {
    let ledger = tempfile::tempdir().unwrap();
    let closure = tempfile::tempdir().unwrap();
    for i in 0..1000 {
        fs::create_dir(closure.path().join(i.to_string())).unwrap();
    }
    fs::create_dir(ledger.path().join("check-cache")).unwrap();
    symlink(closure.path(), ledger.path().join("check-cache/checks")).unwrap();
    let start = Instant::now();
    for _ in 0..25 {
        let mut watch = Watch::new_rebuilds();
        watch.refresh(ledger.path());
        assert!(watch.watcher.is_some());
    }
    println!(
        "25 manual watcher registrations, 1000-dir cache symlink: {:.3} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
}
