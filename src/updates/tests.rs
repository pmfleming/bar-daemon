use std::{fs, os::unix::fs::symlink, time::Duration};

use shelllist_daemon_core::{AtomicFilePolicy, write_bytes_atomic};
use shelllist_update_status::Candidate;
use tempfile::tempdir;
use tokio::time::timeout;

use super::*;

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
fn malformed_unsupported_oversized_and_missing_candidate_records_fail_closed() {
    let root = tempdir().unwrap();
    let directory = root.path().join("updates");
    let good = Readiness::new(1, Some(candidate(root.path(), "good", 123)));
    publish(&directory, &good);
    let path = directory.join("delayed").join(FILE_NAME);
    let value = serde_json::to_value(&good).unwrap();
    use update_worker::job::{Job, Outcome, Phase, Status};
    let (_held, mut job) = Status::start(&directory, &Job::Check(false))
        .unwrap()
        .unwrap();
    job.phase(Phase::Ready).unwrap();
    job.finish_report(Outcome::code(0)).unwrap();
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

async fn next_state(
    events: &mut crate::state::DomainReceivers,
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
