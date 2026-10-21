use super::{history, manual, watch};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

pub(super) fn report(directory: &Path, index: u64, code: i32, changed: bool) -> Value {
    let started = crate::time::unix_ms() / 1000 - 60 + index;
    let stem = format!("rebuild-20261021T103443Z-{index}");
    json!({"schema_version":1,"status":if code == 0 {"completed"} else {"failed"},
        "started_at":started,"finished_at":started + 1,"exit_code":code,"deployed":code == 0,
        "system_before":"/nix/store/old-system", "system_after":if changed {"/nix/store/new-system"} else {"/nix/store/old-system"},
        "log":directory.join(format!("{stem}.{}.log", if code == 0 {"success"} else {"failed"})),
        "run_id":format!("{started}:{stem}"),"failed_stage":if code == 0 {Value::Null} else {json!("Building the system")},
        "error":null,"stages":[{"name":"Building the system","seconds":1.5}]})
}
pub(super) fn publish(directory: &Path, reports: &[Value]) {
    let text = reports
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(directory.join("history.jsonl"), text + "\n").unwrap();
}
#[test]
fn imports_private_rebuild_history_without_worker_records_or_source_writes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    publish(root, &[report(root, 1, 0, true), report(root, 2, 0, false)]);
    let original = fs::read(root.join("history.jsonl")).unwrap();
    let catalog = history::load_catalog(&root.join("absent-worker"), Some(root));
    assert!(catalog.available);
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    assert_eq!(catalog.entries.len(), 2);
    assert_eq!(catalog.entries[0].outcome, "current");
    assert_eq!(catalog.entries[1].outcome, "activated");
    assert_eq!(catalog.entries[1].title, "System rebuilt successfully");
    let id = catalog.entries[1].record.job.id.clone();
    assert!(shelllist_update_status::valid_run_id(&id));
    let page = history::page(
        catalog,
        history::Query {
            run_id: Some(id),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page["history"]["entries"].as_array().unwrap().len(), 1);
    assert!(page["history"]["entries"][0].get("manual_log").is_none());
    assert_eq!(fs::read(root.join("history.jsonl")).unwrap(), original);
    assert!(!root.join("absent-worker").exists());
}
#[test]
fn preserves_activation_when_followup_fails_and_retains_failure_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut partial = report(root, 2, 1, true);
    partial["deployed"] = json!(true);
    publish(
        root,
        &[
            report(root, 1, 1, false),
            partial,
            report(root, 3, 130, false),
        ],
    );
    let catalog = history::load_catalog(&root.join("missing"), Some(root));
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    assert_eq!(
        catalog
            .entries
            .iter()
            .map(|e| e.outcome.as_str())
            .collect::<Vec<_>>(),
        ["interrupted", "partial", "failed"]
    );
    assert_eq!(catalog.entries[1].deployed, Some(true));
    assert_eq!(
        catalog.entries[1].failed_stage.as_deref(),
        Some("Building the system")
    );
    assert_eq!(catalog.entries[2].deployed, Some(false));
    let result = history::page(
        history::load_catalog(&root.join("missing"), Some(root)),
        history::Query {
            query: "partial".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        result["history"]["total"], 1,
        "outcome codes are searchable even without the word in the title"
    );
    assert_eq!(result["history"]["entries"][0]["deployed"], true);
    assert_eq!(
        result["history"]["entries"][0]["failed_stage"],
        "Building the system"
    );
    assert_eq!(
        catalog.entries[1].record.job.phase,
        shelllist_update_status::Phase::Activated
    );
    assert!(
        catalog.entries[2]
            .record
            .job
            .error
            .as_ref()
            .unwrap()
            .contains("Building")
    );
}
#[test]
fn deployment_requires_a_recorded_after_system_and_only_known_differences_count_as_changed() {
    let directory = tempfile::tempdir().unwrap();
    for (before, after, outcome, changed) in [
        (None, None, "completed", false),
        (Some("/nix/store/old"), None, "completed", false),
        (None, Some("/nix/store/new"), "activated", false),
        (
            Some("/nix/store/same"),
            Some("/nix/store/same"),
            "current",
            false,
        ),
        (
            Some("/nix/store/old"),
            Some("/nix/store/new"),
            "activated",
            true,
        ),
    ] {
        let mut value = report(directory.path(), 1, 0, true);
        value["system_before"] = json!(before);
        value["system_after"] = json!(after);
        publish(directory.path(), &[value]);
        let catalog = manual::load(directory.path()).unwrap();
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        assert_eq!(catalog.entries[0].outcome, outcome);
        assert_eq!(catalog.entries[0].record.components[0].changed, changed);
    }
}

#[test]
fn terminal_publication_wins_over_live_marker_and_abandoned_markers_are_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let complete = report(root, 1, 0, true);
    let running_log = complete["log"]
        .as_str()
        .unwrap()
        .replace(".success.log", ".running.log");
    let stat = fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    let mut running = json!({"schema_version":1,"status":"running","started_at":complete["started_at"],"log":running_log,
        "pid":std::process::id(),"boot_id":fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim(),"process_start":fields[19]});
    let marker = format!("{running_log}.run.json");
    fs::write(&marker, serde_json::to_vec(&running).unwrap()).unwrap();
    let catalog = manual::load(root).unwrap();
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    assert_eq!(catalog.entries[0].outcome, "running");
    running["boot_id"] = json!("another-boot");
    fs::write(&marker, serde_json::to_vec(&running).unwrap()).unwrap();
    assert_eq!(
        manual::load(root).unwrap().entries[0].outcome,
        "interrupted"
    );
    publish(root, &[complete]);
    let catalog = manual::load(root).unwrap();
    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(catalog.entries[0].outcome, "activated");
}
#[test]
fn rejects_untrusted_sources_and_log_paths_and_bounds_reads() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for field in ["log", "run_id", "schema_version"] {
        let mut invalid = report(root, 1, 0, true);
        invalid[field] = if field == "schema_version" {
            json!(2)
        } else {
            json!("../outside")
        };
        publish(root, &[invalid]);
        let catalog = manual::load(root).unwrap();
        assert!(catalog.entries.is_empty());
        assert!(!catalog.errors.is_empty());
    }
    fs::write(root.join("history.jsonl"), "{}\n".repeat(1001)).unwrap();
    assert!(
        manual::load(root).is_err(),
        "line count is bounded before collecting all lines"
    );
    fs::remove_file(root.join("history.jsonl")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", root.join("history.jsonl")).unwrap();
    assert!(!manual::load(root).unwrap().errors.is_empty());
    assert!(manual::read_log(&root.join("history.jsonl")).is_err());
    let log = root.join("rebuild-test.success.log");
    fs::write(
        &log,
        (0..150).map(|i| format!("line {i}\n")).collect::<String>(),
    )
    .unwrap();
    let (text, truncated) = manual::read_log(&log).unwrap();
    assert!(truncated);
    assert_eq!(text.lines().count(), 100);
    assert!(text.ends_with("line 149"));
    fs::write(&log, "a".repeat(100000)).unwrap();
    let (text, truncated) = manual::read_log(&log).unwrap();
    assert!(truncated && text.len() <= 32768);
    fs::set_permissions(&log, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(manual::read_log(&log).is_err());
}
#[tokio::test]
async fn rebuild_watch_ignores_log_chatter_but_observes_terminal_publication() {
    let directory = tempfile::tempdir().unwrap();
    let mut watch = watch::Watch::new_rebuilds();
    watch.refresh(directory.path());
    fs::write(
        directory.path().join("rebuild-test.running.log"),
        "build output",
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), watch.wait())
            .await
            .is_err()
    );
    publish(directory.path(), &[report(directory.path(), 1, 0, true)]);
    tokio::time::timeout(Duration::from_secs(2), watch.wait())
        .await
        .unwrap();
}
