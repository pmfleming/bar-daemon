//! Read-only status from the privileged update worker. Never start privileged
//! work or treat stale PID files as proof of an active update.
use std::{fs, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use shelllist_update_status::{JOB_MAX_BYTES, JobState, JobStatus};

use crate::model::UpdateJob;

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
    fs::File::open(path)?
        .take(JOB_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let record = JobStatus::decode(&bytes).context("Decode worker status")?;
    let interrupted = record.status == JobState::Running && !is_running(&record);
    Ok(UpdateJob {
        name: name.into(),
        operation: record.operation,
        status: if interrupted {
            "interrupted".into()
        } else {
            record.status.as_str().into()
        },
        phase: record.phase.as_str().into(),
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

fn is_running(record: &JobStatus) -> bool {
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
        json!({ "schema_version": 1, "id": "test-job", "updated_at": 1, "operation": "run-delayed", "status": "running", "phase": "building",
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
}
