use super::{EventPolicy, Watch};
use notify::{
    Event, EventKind,
    event::{AccessKind, AccessMode, CreateKind, Flag, ModifyKind, RemoveKind, RenameMode},
};
use shelllist_daemon_core::watch::DEFAULT_WATCH_LIMIT;
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

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

#[test]
fn event_policy_distinguishes_ledger_changes_and_topology() {
    let directory = tempfile::tempdir().unwrap();
    let manual = EventPolicy {
        directory: directory.path().to_owned(),
        rebuilds_only: true,
        recursive: false,
        registered: Arc::from([directory.path().join("removed")]),
    };
    for (name, relevant) in [
        ("history.jsonl", true),
        ("rebuild.run.json", true),
        ("build.log", false),
        ("check-cache", false),
    ] {
        let event = Event::new(EventKind::Any).add_path(directory.path().join(name));
        assert_eq!(manual.relevant(&event), relevant);
        assert!(!manual.topology_changed(&event));
    }
    let parent = Event::new(EventKind::Any).add_path(directory.path().parent().unwrap().into());
    assert!(manual.relevant(&parent));
    let worker = EventPolicy {
        rebuilds_only: false,
        recursive: true,
        ..manual
    };
    for kind in [
        EventKind::Create(CreateKind::Folder),
        EventKind::Remove(RemoveKind::Folder),
        EventKind::Any,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
    ] {
        let event = Event::new(kind).add_path(worker.registered[0].clone());
        assert!(worker.topology_changed(&event)); // Includes a removed registered directory.
    }
    let rename = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)));
    assert!(worker.topology_changed(&rename.clone().add_path(directory.path().to_owned())));
    assert!(!worker.topology_changed(&rename.add_path(directory.path().join("missing-file"))));
}

#[test]
fn overflow_and_errors_survive_a_full_queue_but_reads_do_not_invalidate() {
    let policy = EventPolicy {
        directory: "/ledger".into(),
        rebuilds_only: true,
        recursive: false,
        registered: Arc::from([]),
    };
    let (tx, mut rx) = mpsc::channel(1);
    let rebuild = AtomicBool::new(false);
    for mode in [AccessMode::Read, AccessMode::Write] {
        policy.signal(
            &tx,
            &rebuild,
            Ok(Event::new(EventKind::Access(AccessKind::Close(mode)))
                .add_path("/ledger/history.jsonl".into())),
        );
        assert_eq!(rx.try_recv().is_ok(), mode == AccessMode::Write);
    }
    tx.try_send(()).unwrap();
    policy.signal(
        &tx,
        &rebuild,
        Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
    );
    assert!(rebuild.swap(false, Ordering::Relaxed));
    policy.signal(&tx, &rebuild, Err(notify::Error::generic("watch failed")));
    assert!(rebuild.load(Ordering::Relaxed));
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
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
