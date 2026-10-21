//! Read the existing, user-owned `rebuild-report.py` ledger. Never executes a
//! rebuild, changes its outcome, or writes into the privileged worker state.
use super::history::{Catalog, Entry};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use shelllist_update_status::{
    ComponentResult, HISTORY_DAYS, HISTORY_LIMIT, HISTORY_MAX_BYTES, JobState, JobStatus,
    NotificationOwner, Phase, RunRecord, valid_run_id,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) fn directory() -> Option<PathBuf> {
    std::env::var_os("BAR_DAEMON_REBUILD_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
                })
                .map(|state| state.join("nixos-rebuild"))
        })
}
fn trusted_directory(directory: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        value => value?,
    };
    ensure!(
        metadata.is_dir()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o022 == 0,
        "Untrusted manual rebuild directory"
    );
    Ok(true)
}
fn open_owned(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    let file = File::from(
        open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o022 == 0,
        "Untrusted manual rebuild file"
    );
    Ok(file)
}
fn bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_owned(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "Manual rebuild file exceeds read limit"
    );
    Ok(bytes)
}
#[derive(Deserialize)]
struct Report {
    schema_version: u32,
    status: String,
    started_at: u64,
    finished_at: Option<u64>,
    exit_code: Option<i32>,
    deployed: Option<bool>,
    system_before: Option<String>,
    system_after: Option<String>,
    failed_stage: Option<String>,
    error: Option<String>,
    log: PathBuf,
    run_id: Option<String>,
    pid: Option<u32>,
    boot_id: Option<String>,
    process_start: Option<String>,
}
fn entry(directory: &Path, report: Report) -> Result<Entry> {
    ensure!(
        report.schema_version == 1 && report.started_at > 0 && report.started_at < 253402300800,
        "Unsupported rebuild report"
    );
    ensure!(
        report.log.parent() == Some(directory),
        "Rebuild log is outside its ledger directory"
    );
    let filename = report
        .log
        .file_name()
        .and_then(|s| s.to_str())
        .context("Missing rebuild log name")?;
    let stem = [".running.log", ".success.log", ".failed.log"]
        .iter()
        .find_map(|suffix| filename.strip_suffix(suffix))
        .context("Invalid rebuild log suffix")?;
    ensure!(
        stem.starts_with("rebuild-") && valid_run_id(stem) && stem.len() < 100,
        "Invalid rebuild log name"
    );
    ensure!(
        report
            .run_id
            .as_ref()
            .is_none_or(|id| id == &format!("{}:{stem}", report.started_at)),
        "Rebuild identity mismatch"
    );
    ensure!(
        report
            .failed_stage
            .as_ref()
            .is_none_or(|stage| !stage.trim().is_empty()
                && stage.len() <= 512
                && !stage.chars().any(char::is_control)),
        "Invalid failed rebuild stage"
    );
    let id = format!("manual-{}-{stem}", report.started_at);
    let running = report.status == "running";
    let status = match report.status.as_str() {
        "running" => JobState::Running,
        "completed" if report.exit_code == Some(0) => JobState::Completed,
        "failed" if report.exit_code.is_some_and(|c| c != 0) => {
            if report.exit_code == Some(130) {
                JobState::Interrupted
            } else {
                JobState::Failed
            }
        }
        "interrupted" => JobState::Interrupted,
        _ => anyhow::bail!("Invalid rebuild outcome"),
    };
    ensure!(
        running
            || report
                .finished_at
                .is_some_and(|t| t > 0 && t < 253402300800),
        "Missing rebuild completion time"
    );
    ensure!(
        !running
            || (report.pid.is_some_and(|pid| pid > 0)
                && report.boot_id.is_some()
                && report.process_start.is_some()),
        "Missing rebuild process identity"
    );
    let before = report.system_before.filter(|s| !s.is_empty());
    let after = report.system_after.filter(|s| !s.is_empty());
    for system in [&before, &after].into_iter().flatten() {
        ensure!(
            system.len() <= 512 && system.starts_with("/nix/store/") && !system.contains('\n'),
            "Invalid recorded system identity"
        );
    }
    let changed =
        report.deployed == Some(true) && before.is_some() && after.is_some() && before != after;
    let activated =
        report.deployed == Some(true) && after.is_some() && (before.is_none() || changed);
    let unchanged = report.deployed == Some(true) && before.is_some() && before == after;
    let phase = if running {
        Phase::Starting
    } else if activated {
        Phase::Activated
    } else if status == JobState::Completed && unchanged {
        Phase::Current
    } else {
        Phase::Checking
    };
    let mut job = JobStatus {
        schema_version: 1,
        id,
        operation: "manual-rebuild".into(),
        status,
        phase,
        started_at: report.started_at,
        updated_at: report.finished_at.unwrap_or(report.started_at),
        finished_at: report.finished_at,
        exit_code: report.exit_code,
        pid: report.pid.unwrap_or(0),
        boot_id: report.boot_id.unwrap_or_default(),
        process_start: report.process_start.unwrap_or_default(),
        error: report.error.map(|s| s.chars().take(4096).collect()),
    };
    if job.status == JobState::Failed && job.error.is_none() {
        job.error = Some(format!(
            "Rebuild failed during {}",
            report
                .failed_stage
                .as_deref()
                .unwrap_or("an unrecorded stage")
                .chars()
                .take(512)
                .collect::<String>()
        ));
    }
    let interrupted = running && !super::jobs::is_running(&job);
    let (outcome, title) = if interrupted || status == JobState::Interrupted {
        (
            "interrupted",
            "Manual rebuild interrupted — completion unknown",
        )
    } else if running {
        ("running", "Manual rebuild in progress")
    } else if status == JobState::Failed && report.deployed == Some(true) {
        ("partial", "System rebuilt; follow-up failed")
    } else if status == JobState::Failed {
        ("failed", "Manual rebuild failed")
    } else if activated {
        ("activated", "System rebuilt successfully")
    } else if unchanged {
        ("current", "Manual rebuild completed — system unchanged")
    } else {
        (
            "completed",
            "Manual rebuild completed — activation not recorded",
        )
    };
    let components = vec![ComponentResult {
        name: "NixOS system".into(),
        before,
        after,
        changed,
        phase: phase.as_str().into(),
        error: None,
        incident: None,
    }];
    Ok(Entry {
        record: RunRecord {
            schema_version: 1,
            lane: "system".into(),
            notification_owner: NotificationOwner::Desktop,
            job,
            components,
        },
        outcome: outcome.into(),
        title: title.into(),
        failed_stage: report.failed_stage,
        deployed: report.deployed,
        issues: Vec::new(),
        manual_log: Some(directory.join(filename)),
    })
}

pub(super) fn load(directory: &Path) -> Result<Catalog> {
    if !trusted_directory(directory)? {
        return Ok(Catalog::default());
    }
    let mut catalog = Catalog {
        available: true,
        ..Default::default()
    };
    let mut records = BTreeMap::new();
    let cutoff = (crate::time::unix_ms() / 1000).saturating_sub(HISTORY_DAYS * 86400);
    let mut add = |raw: &[u8], terminal: bool| -> Result<()> {
        ensure!(
            raw.len() <= HISTORY_MAX_BYTES,
            "Rebuild record exceeds read limit"
        );
        let report: Report = serde_json::from_slice(raw)?;
        ensure!(
            !terminal || report.status != "running",
            "Running record in completed rebuild ledger"
        );
        let entry = entry(directory, report)?;
        if entry.outcome == "running"
            || entry
                .record
                .job
                .finished_at
                .unwrap_or(entry.record.job.updated_at)
                >= cutoff
        {
            // Terminal publication precedes marker removal. Never regress to running.
            records.entry(entry.record.job.id.clone()).or_insert(entry);
        }
        Ok(())
    };
    let history = directory.join("history.jsonl");
    match bytes(&history, 16 * 1024 * 1024) {
        Ok(raw) => {
            for (index, line) in raw
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .enumerate()
            {
                ensure!(index < HISTORY_LIMIT, "Too many rebuild records");
                if let Err(error) = add(line, true)
                    && catalog.errors.len() < 8
                {
                    catalog.errors.push(format!("Rebuild history: {error:#}"));
                }
            }
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => catalog.errors.push(format!("Rebuild history: {error:#}")),
    }
    let mut markers = 0;
    for (index, item) in std::fs::read_dir(directory)?.enumerate() {
        ensure!(index < 4096, "Rebuild directory exceeds read limit");
        let path = item?.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rebuild-") && n.ends_with(".running.log.run.json"))
        {
            continue;
        }
        markers += 1;
        ensure!(markers <= 128, "Too many rebuild markers");
        let result = bytes(&path, HISTORY_MAX_BYTES).and_then(|raw| {
            let report: Report = serde_json::from_slice(&raw)?;
            ensure!(
                report.status == "running" && path == format!("{}.run.json", report.log.display()),
                "Rebuild marker identity mismatch"
            );
            add(&raw, false)
        });
        if let Err(error) = result
            && catalog.errors.len() < 8
        {
            catalog.errors.push(format!("Rebuild marker: {error:#}"));
        }
    }
    catalog.entries = records.into_values().collect();
    Ok(catalog)
}

pub(super) fn read_log(path: &Path) -> Result<(String, bool)> {
    ensure!(
        trusted_directory(path.parent().context("Missing log directory")?)?,
        "Rebuild log directory missing"
    );
    let mut file = open_owned(path)?;
    let length = file.metadata()?.len();
    let truncated = length > 32768;
    file.seek(SeekFrom::Start(length.saturating_sub(32768)))?;
    let mut bytes = Vec::new();
    file.take(32768).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // Do not display a partial first line from a byte-truncated tail.
    let lines: Vec<_> = text.lines().skip(usize::from(truncated)).collect();
    let start = lines.len().saturating_sub(100);
    Ok((lines[start..].join("\n"), truncated || start > 0))
}
