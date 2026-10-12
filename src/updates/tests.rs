use std::{fs, os::unix::fs::symlink, time::Duration};

use shelllist_daemon_core::{AtomicFilePolicy, write_bytes_atomic};
use shelllist_update_status::Candidate;
use tempfile::tempdir;
use tokio::{sync::broadcast, time::timeout};

use super::*;
use crate::state::DomainEvent;

fn candidate(root: &Path, revision: &str, created_at: u64) -> Candidate {
    let system = root.join(format!("system-{revision}"));
    fs::create_dir_all(&system).unwrap();
    Candidate {
        revision: revision.into(),
        source_hash: "a".repeat(64),
        lock_hash: "b".repeat(64),
        base_hash: "c".repeat(64),
        created_at,
        auto_apply: created_at % 2 == 0,
        system,
    }
}

fn publish(directory: &Path, record: &Readiness) {
    write_bytes_atomic(
        &directory.join("delayed").join(FILE_NAME),
        &record.encode().unwrap(),
        AtomicFilePolicy::DURABLE,
    )
    .unwrap();
}

#[test]
fn reads_one_complete_record_and_never_assembles_legacy_fragments() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    let lane = directory.join("delayed");
    fs::create_dir_all(&lane).unwrap();
    for name in [
        "ready-flake.lock",
        "ready-revision",
        "ready-base-hash",
        "ready-created-at",
        "auto-apply",
    ] {
        fs::write(lane.join(name), "legacy-garbage").unwrap();
    }
    symlink("/missing-legacy-system", lane.join("system")).unwrap();
    let state = read_state(&directory).unwrap();
    assert!(
        !state.ready,
        "An older worker needs to publish the new record first"
    );
    assert!(state.error.is_some());

    let candidate = candidate(root.path(), "complete", 123);
    publish(&directory, &Readiness::new(1, Some(candidate.clone())));
    let state = read_state(&directory).unwrap();
    assert!(state.ready);
    assert!(state.error.is_none());
    assert_eq!(state.lanes[0].revision.as_deref(), Some("complete"));
    assert_eq!(
        state.lanes[0].base_hash.as_deref(),
        Some(candidate.base_hash.as_str())
    );
    assert_eq!(state.lanes[0].created_at, Some(123));
    assert!(
        !state.lanes[0].auto_apply,
        "Legacy marker must not override this record"
    );
    assert_eq!(state.lanes[0].system.as_deref(), candidate.system.to_str());
    fs::remove_file(lane.join("system")).unwrap();
    assert!(read_state(&directory).unwrap().ready);

    // Invalidation wins even while every old readiness fragment still exists.
    publish(&directory, &Readiness::new(2, None));
    let state = read_state(&directory).unwrap();
    assert!(!state.ready);
    assert!(state.lanes[0].revision.is_none());
    assert!(state.error.is_none());
    fs::rename(&lane, directory.join("fast")).unwrap();
    assert!(
        !read_state(&directory).unwrap().ready,
        "Obsolete lanes are not read"
    );
}

#[test]
fn malformed_unsupported_oversized_and_missing_candidate_records_fail_closed() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    let good = Readiness::new(1, Some(candidate(root.path(), "good", 123)));
    publish(&directory, &good);
    let path = directory.join("delayed").join(FILE_NAME);
    let value = serde_json::to_value(&good).unwrap();
    fs::create_dir(directory.join("jobs")).unwrap();
    fs::write(
        directory.join("jobs/system.json"),
        serde_json::json!({
            "schema_version": 1, "operation": "check", "status": "completed", "phase": "ready",
            "started_at": 1, "finished_at": 2, "exit_code": 0, "pid": 1,
            "boot_id": "past-boot", "process_start": "past-process", "error": null,
        })
        .to_string(),
    )
    .unwrap();
    for (pointer, replacement) in [
        ("/schema_version", serde_json::json!(2)),
        ("/generation", serde_json::json!(0)),
        ("/candidate", serde_json::Value::Null),
        ("/ready", serde_json::json!(false)),
        ("/candidate/base_hash", serde_json::json!("bad")),
        ("/candidate/system", serde_json::json!("relative")),
        (
            "/candidate/system",
            serde_json::json!(root.path().join("missing")),
        ),
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let state = read_state(&directory).unwrap();
        assert!(!state.ready, "{pointer}");
        assert!(state.error.is_some(), "{pointer}");
        assert!(state.available);
        assert_eq!(
            state.jobs[0].status, "completed",
            "Readiness errors must not hide job status"
        );
        assert!(
            state.lanes[0].revision.is_none(),
            "Never expose a partial candidate"
        );
    }
    for invalid in [b"{".to_vec(), vec![b' '; MAX_BYTES + 1]] {
        fs::write(&path, invalid).unwrap();
        let state = read_state(&directory).unwrap();
        assert!(!state.ready);
        assert!(state.error.is_some());
    }
    fs::remove_file(&path).unwrap();
    symlink(root.path().join("missing"), &path).unwrap();
    assert!(read_state(&directory).unwrap().error.is_some());
}

#[test]
fn interrupted_publication_leaves_old_record_until_commit_or_invalidation() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    publish(
        &directory,
        &Readiness::new(1, Some(candidate(root.path(), "old", 1))),
    );
    fs::write(directory.join("delayed/.readiness-uncommitted"), "{partial").unwrap();
    assert_eq!(
        read_state(&directory).unwrap().lanes[0].revision.as_deref(),
        Some("old")
    );
    publish(&directory, &Readiness::new(2, None));
    fs::remove_dir(root.path().join("system-old")).unwrap();
    let state = read_state(&directory).unwrap();
    assert!(!state.ready);
    assert!(state.error.is_none());
}

#[test]
fn concurrent_atomic_publications_never_mix_candidate_fields() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    let records = [
        Readiness::new(1, Some(candidate(root.path(), "one", 1))),
        Readiness::new(2, Some(candidate(root.path(), "two", 2))),
    ];
    publish(&directory, &records[0]);
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            for index in 0..80 {
                publish(&directory, &records[index % 2]);
            }
        });
        for _ in 0..500 {
            let state = read_state(&directory).unwrap();
            assert!(state.ready && state.error.is_none());
            let lane = &state.lanes[0];
            let expected = records
                .iter()
                .filter_map(|record| record.candidate.as_ref())
                .find(|candidate| Some(candidate.revision.as_str()) == lane.revision.as_deref())
                .unwrap();
            assert_eq!(lane.created_at, Some(expected.created_at));
            assert_eq!(lane.auto_apply, expected.auto_apply);
            assert_eq!(lane.system.as_deref(), expected.system.to_str());
        }
        writer.join().unwrap();
    });
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
    publish(
        &directory,
        &Readiness::new(1, Some(candidate(root.path(), "first", 123))),
    );
    next_state(&mut events, |s| s.ready).await;

    let replacement = root.path().join("replacement");
    publish(
        &replacement,
        &Readiness::new(2, Some(candidate(root.path(), "second", 456))),
    );
    fs::rename(&directory, root.path().join("retired")).unwrap();
    fs::rename(&replacement, &directory).unwrap();
    next_state(&mut events, |s| {
        s.ready && s.lanes[0].revision.as_deref() == Some("second")
    })
    .await;
    // Subsequent atomic replacement must arrive through the new inode's watch.
    publish(
        &directory,
        &Readiness::new(3, Some(candidate(root.path(), "third", 789))),
    );
    next_state(&mut events, |s| {
        s.ready && s.lanes[0].revision.as_deref() == Some("third")
    })
    .await;

    fs::remove_dir_all(&directory).unwrap();
    next_state(&mut events, |s| !s.available).await;
    publish(&directory, &Readiness::new(4, None));
    next_state(&mut events, |s| s.available && !s.ready).await;
    publish(
        &directory,
        &Readiness::new(5, Some(candidate(root.path(), "fourth", 999))),
    );
    next_state(&mut events, |s| s.ready).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
