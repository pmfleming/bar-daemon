//! Read-only status from the privileged update worker. Never start privileged
//! work or treat stale PID files as proof of an active update.
use std::{fs, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::model::UpdateJob;

#[derive(Deserialize)]
struct Record {
    schema_version: u32,
    operation: String,
    status: String,
    phase: String,
    started_at: u64,
    finished_at: Option<u64>,
    exit_code: Option<i32>,
    pid: u32,
    boot_id: String,
    process_start: String,
    error: Option<String>,
}

pub(super) fn read(directory: &Path) -> Vec<UpdateJob> {
    let mut jobs: Vec<_> = ["system", "ai-tools", "ai-tools-stale"]
        .into_iter()
        .filter_map(|name| {
            let path = directory.join("jobs").join(format!("{name}.json"));
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                _ => Some(read_one(&path, name).unwrap_or_else(|error| UpdateJob {
                    name: name.into(),
                    status: "failed".into(),
                    error: Some(format!("Update status is unavailable: {error:#}")),
                    ..Default::default()
                })),
            }
        })
        .collect();
    // A successful update supersedes an older stale-check warning immediately,
    // without waiting for the next three-hour stale-check timer.
    let fresh = jobs
        .iter()
        .find(|j| {
            j.name == "ai-tools"
                && j.status == "completed"
                && matches!(j.phase.as_str(), "activated" | "current")
        })
        .and_then(|j| j.finished_at);
    for job in &mut jobs {
        if job.name == "ai-tools-stale"
            && job.phase == "stale"
            && fresh.is_some_and(|t| t >= job.finished_at.unwrap_or(u64::MAX))
        {
            job.phase = "current".into();
        }
    }
    jobs
}

fn read_one(path: &Path, name: &str) -> Result<UpdateJob> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "Worker status is not a regular file"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(16_385).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16_384, "Worker status exceeds size limit");
    let record: Record = serde_json::from_slice(&bytes).context("Decode worker status")?;
    ensure!(
        record.schema_version == 1,
        "Unsupported update-worker status version"
    );
    ensure!(
        matches!(
            record.status.as_str(),
            "running" | "completed" | "failed" | "interrupted"
        ),
        "Unknown worker status"
    );
    let interrupted = record.status == "running" && !is_running(&record);
    Ok(UpdateJob {
        name: name.into(),
        operation: record.operation,
        status: if interrupted {
            "interrupted".into()
        } else {
            record.status
        },
        phase: record.phase,
        started_at: record.started_at,
        finished_at: record.finished_at,
        exit_code: record.exit_code,
        error: if interrupted {
            Some("Update worker stopped without a terminal status; inspect the service journal. Pending transactions recover on the next run.".into())
        } else {
            record.error
        },
    })
}

fn is_running(record: &Record) -> bool {
    if fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .is_none_or(|boot| boot.trim() != record.boot_id)
    {
        return false;
    }
    fs::read_to_string(format!("/proc/{}/stat", record.pid))
        .ok()
        .is_some_and(|stat| {
            let Some((_, fields)) = stat.rsplit_once(')') else {
                return false;
            };
            let fields: Vec<_> = fields.split_whitespace().collect();
            fields
                .first()
                .is_some_and(|state| !matches!(*state, "Z" | "X"))
                && fields
                    .get(19)
                    .is_some_and(|start| *start == record.process_start)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record() -> serde_json::Value {
        json!({ "schema_version": 1, "operation": "run-delayed", "status": "running", "phase": "building",
            "started_at": 1, "finished_at": null, "exit_code": null, "pid": std::process::id(),
            "boot_id": fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim(),
            "process_start": fs::read_to_string("/proc/self/stat").unwrap().rsplit_once(')').unwrap().1.split_whitespace().nth(19).unwrap(), "error": null })
    }
    #[test]
    fn crash_reboot_and_pid_reuse_cannot_appear_as_running() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("jobs")).unwrap();
        let path = root.path().join("jobs/system.json");
        let mut value = record();
        fs::write(&path, value.to_string()).unwrap();
        assert_eq!(read(root.path())[0].status, "running");
        value["process_start"] = json!("not-this-process");
        fs::write(&path, value.to_string()).unwrap();
        assert_eq!(read(root.path())[0].status, "interrupted");
        value = record();
        value["boot_id"] = json!("old-boot");
        fs::write(&path, value.to_string()).unwrap();
        assert_eq!(read(root.path())[0].status, "interrupted");
        value["status"] = json!("completed");
        fs::write(&path, value.to_string()).unwrap();
        assert_eq!(read(root.path())[0].status, "completed");
    }
    #[test]
    fn rejects_malformed_oversized_and_symlinked_status_without_losing_other_jobs() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("jobs")).unwrap();
        let path = root.path().join("jobs/system.json");
        for data in ["broken".to_string(), " ".repeat(16_385)] {
            fs::write(&path, data).unwrap();
            assert_eq!(read(root.path())[0].status, "failed");
        }
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/missing", &path).unwrap();
        assert_eq!(read(root.path())[0].status, "failed");
    }
    #[test]
    fn successful_ai_update_clears_only_older_staleness() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("jobs")).unwrap();
        let mut stale = record();
        stale["status"] = json!("completed");
        stale["phase"] = json!("stale");
        stale["finished_at"] = json!(100);
        fs::write(
            root.path().join("jobs/ai-tools-stale.json"),
            stale.to_string(),
        )
        .unwrap();
        let mut fresh = stale.clone();
        fresh["phase"] = json!("activated");
        fresh["finished_at"] = json!(101);
        fs::write(root.path().join("jobs/ai-tools.json"), fresh.to_string()).unwrap();
        assert_eq!(read(root.path())[1].phase, "current");
        stale["finished_at"] = json!(102);
        fs::write(
            root.path().join("jobs/ai-tools-stale.json"),
            stale.to_string(),
        )
        .unwrap();
        assert_eq!(read(root.path())[1].phase, "stale");
    }
}
