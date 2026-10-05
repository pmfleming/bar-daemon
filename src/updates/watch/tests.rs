use std::fs;

use notify::{Event, event::Flag};
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn read_events_are_quiet_and_recovery_survives_coalescing() {
    let mut watch = Watch::new();
    for kind in [
        AccessKind::Open(AccessMode::Any),
        AccessKind::Read,
        AccessKind::Close(AccessMode::Read),
    ] {
        signal(
            &watch.tx,
            &watch.rebuild,
            Ok(Event::new(EventKind::Access(kind))),
        );
    }
    assert!(watch.rx.try_recv().is_err());
    for _ in 0..100 {
        signal(
            &watch.tx,
            &watch.rebuild,
            Ok(Event::new(EventKind::Modify(
                notify::event::ModifyKind::Any,
            ))),
        );
    }
    signal(
        &watch.tx,
        &watch.rebuild,
        Err(notify::Error::generic("lost watch")),
    );
    assert_eq!(watch.rx.len(), 1);
    assert!(watch.rebuild.load(Ordering::Relaxed));
    watch.wait().await;
    assert!(watch.rx.try_recv().is_err());
    // Recovery isn't consumed by draining the wake-up queue.
    assert!(watch.rebuild.swap(false, Ordering::Relaxed));
    signal(
        &watch.tx,
        &watch.rebuild,
        Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
    );
    assert!(watch.rebuild.load(Ordering::Relaxed));
    watch.rx.try_recv().unwrap();
    signal(
        &watch.tx,
        &watch.rebuild,
        Ok(Event::new(EventKind::Access(AccessKind::Close(
            AccessMode::Write,
        )))),
    );
    watch.rx.try_recv().unwrap();
}

#[tokio::test]
async fn real_reads_do_not_trigger_refresh_but_atomic_writes_do() {
    let root = tempdir().unwrap();
    let file = root.path().join("status.json");
    fs::write(&file, "old").unwrap();
    let mut watch = Watch::new();
    watch.refresh(root.path());
    assert!(watch.watcher.is_some());
    for _ in 0..20 {
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
    }
    assert!(
        time::timeout(Duration::from_millis(200), watch.rx.recv())
            .await
            .is_err()
    );
    fs::write(root.path().join("status.new"), "new").unwrap();
    fs::rename(root.path().join("status.new"), &file).unwrap();
    time::timeout(Duration::from_secs(3), watch.wait())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    assert!(
        time::timeout(Duration::from_millis(200), watch.rx.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn retries_missing_watch_and_tracks_parent_and_directory_identity() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    let mut watch = Watch::new();
    // No watch/target can be installed for a relative empty path. A later
    // refresh retries initialization rather than retaining permanent failure.
    watch.refresh(Path::new(""));
    assert!(watch.watcher.is_none());
    watch.refresh(&directory);
    assert_eq!(watch.target.as_ref().unwrap().path, root.path());
    assert!(!watch.target.as_ref().unwrap().recursive);
    fs::create_dir(&directory).unwrap();
    watch.refresh(&directory);
    assert!(watch.target.as_ref().unwrap().recursive);
    let original_inode = watch.target.as_ref().unwrap().inode;
    fs::rename(&directory, root.path().join("retired")).unwrap();
    fs::create_dir(&directory).unwrap();
    watch.refresh(&directory);
    assert_ne!(watch.target.as_ref().unwrap().inode, original_inode);
    assert_eq!(watch.target.as_ref().unwrap().path, directory);
    signal(
        &watch.tx,
        &watch.rebuild,
        Err(notify::Error::generic("lost watch")),
    );
    watch.refresh(&directory);
    assert!(!watch.rebuild.load(Ordering::Relaxed));
    assert!(watch.watcher.is_some());
    // Drain replacement/recovery events before testing the reinstalled watch.
    time::timeout(Duration::from_secs(3), watch.wait())
        .await
        .unwrap();
    fs::write(directory.join("new-status"), "changed").unwrap();
    time::timeout(Duration::from_secs(3), watch.wait())
        .await
        .unwrap();
}
