//! This boundary executes the real worker publishers and the real bar reader.
//! No privileged entrypoint, live state directory or deployment is invoked.
use super::*;
use shelllist_update_status::Candidate;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
};
use update_worker::{
    job::{Job, Outcome, Phase, Status},
    readiness,
};

#[test]
fn history_reads_real_producer_runs_and_rejects_a_cursor_after_new_publication() {
    let root = tempfile::tempdir().unwrap();
    for phase in [Phase::Staged, Phase::Current] {
        let (_lock, mut status) = Status::start(root.path(), &Job::Run).unwrap().unwrap();
        status.phase(phase).unwrap();
        status.finish_report(Outcome::code(0)).unwrap();
    }
    let catalog = history::load_catalog(root.path(), None);
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    assert_eq!(catalog.entries.len(), 2);
    assert_eq!(catalog.entries[0].outcome, "current");
    assert_eq!(catalog.entries[1].outcome, "staged");
    let page = history::page(
        catalog,
        history::Query {
            limit: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let cursor = page["history"]["cursor"].as_str().unwrap().to_owned();
    {
        let (_lock, mut status) = Status::start(root.path(), &Job::Run).unwrap().unwrap();
        status.finish_report(Outcome::code(1)).unwrap();
    }
    assert!(
        history::page(
            history::load_catalog(root.path(), None),
            history::Query {
                cursor: Some(cursor),
                ..Default::default()
            }
        )
        .is_err()
    );
}

fn candidate(root: &Path, name: &str) -> Candidate {
    let system = root.join(name);
    fs::create_dir_all(system.join("bin")).unwrap();
    let executable = system.join("bin/switch-to-configuration");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    Candidate {
        revision: name.into(),
        source_hash: "a".repeat(64),
        lock_hash: "b".repeat(64),
        base_hash: "c".repeat(64),
        created_at: 42,
        auto_apply: false,
        system,
    }
}

#[test]
fn concurrent_actual_publication_is_whole_and_invalidation_precedes_retirement() {
    let root = tempfile::tempdir().unwrap();
    let lane = root.path().join("delayed");
    readiness::publish(&lane, None).unwrap();
    let candidates = [candidate(root.path(), "one"), candidate(root.path(), "two")];
    let barrier = Arc::new(Barrier::new(3));
    let done = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for value in &candidates {
            let barrier = barrier.clone();
            let done = done.clone();
            let lane = lane.clone();
            let lock = root.path().join("publication.lock");
            scope.spawn(move || {
                barrier.wait();
                for iteration in 0..50 {
                    // The production publisher requires its caller's state lock.
                    let held = loop {
                        if let Some(held) = update_worker::fs::lock(&lock, true).unwrap() {
                            break held;
                        }
                        std::thread::yield_now();
                    };
                    readiness::publish(&lane, (iteration % 3 != 0).then(|| value.clone())).unwrap();
                    drop(held);
                }
                done.fetch_add(1, Ordering::Release);
            });
        }
        barrier.wait();
        let mut observations = 0;
        while done.load(Ordering::Acquire) < 2 || observations < 100 {
            let state = read_state(root.path()).unwrap();
            assert!(state.error.is_none(), "{:?}", state.error);
            let lane = &state.lanes[0];
            if lane.ready {
                let expected = candidates
                    .iter()
                    .find(|c| Some(&c.revision) == lane.revision.as_ref())
                    .unwrap();
                assert_eq!(lane.system.as_deref(), expected.system.to_str());
                assert_eq!(lane.created_at, Some(expected.created_at));
            } else {
                assert!(lane.system.is_none());
            }
            observations += 1;
        }
    });
    readiness::publish(&lane, Some(candidates[0].clone())).unwrap();
    readiness::publish(&lane, None).unwrap();
    fs::remove_dir_all(&candidates[0].system).unwrap();
    assert!(!read_state(root.path()).unwrap().ready);
    // A remaining old legacy record/link must never resurrect readiness.
    fs::write(lane.join("ready"), "true").unwrap();
    assert!(!read_state(root.path()).unwrap().ready);
}

#[test]
fn worker_crash_child() {
    let Some(root) = std::env::var_os("BAR_WORKER_CRASH_TEST") else {
        return;
    };
    let root = PathBuf::from(root);
    match std::env::var("BAR_WORKER_CRASH_STAGE").unwrap().as_str() {
        "before" => {
            let _unpublished = Status::new(root.join("jobs/system.json"), &Job::Run).unwrap();
        }
        "running" => {
            let (_lock, mut status) = Status::start(&root, &Job::Run).unwrap().unwrap();
            status.phase(Phase::Building).unwrap();
            // exit skips destructors: the real process identity becomes stale.
            std::process::exit(23);
        }
        "invalidated" => {
            readiness::publish(&root.join("delayed"), None).unwrap();
        }
        _ => panic!("invalid crash stage"),
    }
    std::process::exit(23);
}

#[test]
fn crash_before_publication_preserves_old_record_and_after_publication_marks_interrupted() {
    let root = tempfile::tempdir().unwrap();
    let (held, mut status) = Status::start(root.path(), &Job::Run).unwrap().unwrap();
    status.finish_report(Outcome::code(0)).unwrap();
    drop(held);
    let path = root.path().join("jobs/system.json");
    let old = fs::read(&path).unwrap();
    let value = candidate(root.path(), "system");
    readiness::publish(&root.path().join("delayed"), Some(value.clone())).unwrap();
    for stage in ["before", "running", "invalidated"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "updates::producer_tests::worker_crash_child",
                "--nocapture",
            ])
            .env("BAR_WORKER_CRASH_TEST", root.path())
            .env("BAR_WORKER_CRASH_STAGE", stage)
            .status()
            .unwrap();
        assert_eq!(result.code(), Some(23));
        match stage {
            "before" => assert_eq!(fs::read(&path).unwrap(), old),
            "running" => assert_eq!(jobs::read(root.path())[0].status, "interrupted"),
            "invalidated" => {
                assert!(value.system.exists());
                assert!(!read_state(root.path()).unwrap().ready);
            }
            _ => unreachable!(),
        }
    }
}
