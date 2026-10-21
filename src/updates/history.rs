//! Bounded, read-only catalog of producer-owned run records. No deployment effects.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shelllist_update_status::{
    HISTORY_LIMIT, HISTORY_MAX_BYTES, JobState, RunRecord, valid_run_id,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::Path,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Query {
    pub query: String,
    pub cursor: Option<String>,
    pub run_id: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Clone, Serialize)]
pub(crate) struct Entry {
    #[serde(flatten)]
    pub record: RunRecord,
    pub outcome: String,
    pub title: String,
    pub failed_stage: Option<String>,
    pub deployed: Option<bool>,
    pub issues: Vec<super::issues::RunIssue>,
    #[serde(skip)]
    pub manual_log: Option<std::path::PathBuf>,
}
#[derive(Default)]
pub(crate) struct Catalog {
    pub entries: Vec<Entry>,
    pub available: bool,
    pub errors: Vec<String>,
    pub open_issues: Vec<super::issues::OpenIssue>,
}

pub(crate) fn load(root: &Path) -> Result<Catalog> {
    let directory = root.join("history");
    let metadata = match std::fs::symlink_metadata(&directory) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Catalog::default()),
        value => value?,
    };
    ensure!(
        metadata.is_dir() && metadata.mode() & 0o022 == 0,
        "Untrusted update history directory"
    );
    let owner = metadata.uid();
    let mut result = Catalog {
        available: true,
        ..Default::default()
    };
    for (count, item) in std::fs::read_dir(&directory)?.enumerate() {
        ensure!(
            count < HISTORY_LIMIT + 128,
            "Update history directory exceeds record limit"
        );
        let path = item?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let read = || -> Result<RunRecord> {
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.uid() == owner && metadata.mode() & 0o022 == 0,
                "Untrusted history file"
            );
            let bytes = shelllist_daemon_core::read_bytes_bounded(&path, HISTORY_MAX_BYTES as u64)?
                .context("History record disappeared")?;
            let record = RunRecord::decode(&bytes)?;
            ensure!(
                path.file_stem().and_then(|n| n.to_str()) == Some(&record.job.id),
                "History identity mismatch"
            );
            Ok(record)
        };
        match read() {
            Ok(record) => {
                let interrupted =
                    record.job.status == JobState::Running && !super::jobs::is_running(&record.job);
                let cutoff = (crate::time::unix_ms() / 1000)
                    .saturating_sub(shelllist_update_status::HISTORY_DAYS * 86400);
                if (interrupted || record.job.status != JobState::Running)
                    && record.job.finished_at.unwrap_or(record.job.updated_at) < cutoff
                {
                    continue;
                }
                result.entries.push(Entry {
                    outcome: if interrupted {
                        "interrupted"
                    } else {
                        record.outcome()
                    }
                    .into(),
                    title: if interrupted {
                        "Update interrupted — completion unknown"
                    } else {
                        record.title()
                    }
                    .into(),
                    record,
                    failed_stage: None,
                    deployed: None,
                    issues: Vec::new(),
                    manual_log: None,
                });
            }
            Err(error) => {
                if result.errors.len() < 8 {
                    result.errors.push(format!(
                        "{}: {error:#}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
        }
    }
    result.entries.sort_by(|a, b| {
        (b.record.job.started_at, &b.record.job.id)
            .cmp(&(a.record.job.started_at, &a.record.job.id))
    });
    Ok(result)
}

pub(super) fn load_catalog(root: &Path, manual: Option<&Path>) -> Catalog {
    let mut catalog = Catalog::default();
    let sources = [
        load(root),
        manual
            .map(super::manual::load)
            .unwrap_or_else(|| Ok(Catalog::default())),
    ];
    for source in sources {
        match source {
            Ok(mut source) => {
                catalog.available |= source.available;
                catalog.entries.append(&mut source.entries);
                catalog.errors.append(&mut source.errors);
            }
            Err(error) => catalog.errors.push(format!("Update history: {error:#}")),
        }
    }
    catalog.entries.sort_by(|a, b| {
        (b.record.job.started_at, &b.record.job.id)
            .cmp(&(a.record.job.started_at, &a.record.job.id))
    });
    let mut completed = 0;
    catalog.entries.retain(|entry| {
        if entry.outcome == "running" {
            return true;
        }
        completed += 1;
        completed <= HISTORY_LIMIT
    });
    catalog.open_issues = super::issues::annotate(&mut catalog.entries);
    catalog
}

pub(crate) async fn query(params: Value) -> Result<Value> {
    static READERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let query: Query = serde_json::from_value(params)?;
    let directory = super::state_dir();
    let manual = super::manual::directory();
    let read = async {
        let permit = READERS.acquire().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit; // Cancellation cannot admit unbounded abandoned scans.
            page(load_catalog(&directory, manual.as_deref()), query)
        })
        .await
        .context("Join update history read")?
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), read)
        .await
        .context("Update history read timed out")?
}

pub(crate) fn page(catalog: Catalog, query: Query) -> Result<Value> {
    ensure!(
        query.query.len() <= 512 && (1..=100).contains(&query.limit.unwrap_or(50)),
        "Invalid update history query"
    );
    ensure!(
        query.run_id.as_ref().is_none_or(|id| valid_run_id(id)),
        "Invalid run identity"
    );
    let words: Vec<_> = query
        .query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let mut hasher = DefaultHasher::new();
    serde_json::to_vec(&catalog.entries)?.hash(&mut hasher);
    query.query.hash(&mut hasher);
    query.run_id.hash(&mut hasher);
    let revision = format!("{:016x}", hasher.finish());
    let offset = match query.cursor {
        Some(cursor) => {
            ensure!(cursor.len() < 100, "Invalid history cursor");
            let (rev, offset) = cursor.split_once(':').context("Invalid history cursor")?;
            ensure!(
                rev == revision,
                "Update history changed; refresh before continuing"
            );
            offset.parse::<usize>()?
        }
        None => 0,
    };
    let matching: Vec<_> = catalog
        .entries
        .into_iter()
        .filter(|entry| {
            if let Some(id) = &query.run_id {
                return &entry.record.job.id == id;
            }
            let text = format!(
                "{} {} {} {} {} {} {}",
                entry.outcome,
                entry.title,
                entry.record.lane,
                entry.record.job.operation,
                entry.record.job.id,
                entry.record.job.error.as_deref().unwrap_or_default(),
                serde_json::to_string(&entry.record.components).unwrap_or_default()
            )
            .to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .collect();
    ensure!(offset <= matching.len(), "Invalid history offset");
    let mut end = offset;
    let mut bytes = 0;
    for entry in matching.iter().skip(offset).take(query.limit.unwrap_or(50)) {
        let size = serde_json::to_vec(entry)?.len();
        if end > offset && bytes + size > 128 * 1024 {
            break;
        }
        bytes += size;
        end += 1;
    }
    Ok(json!({"history": {
        "available": catalog.available, "revision": revision, "total": matching.len(),
        "entries": &matching[offset..end], "cursor": (end < matching.len()).then(|| format!("{revision}:{end}")),
        "error": (!catalog.errors.is_empty()).then(|| catalog.errors.join("\n")),
        "coverage": "Manual rebuilds and worker-managed system/AI-tool runs; 90 days / 1,000 completed runs. Manual logs remain private to this user."
    }}))
}

pub(crate) async fn read_log(params: Value) -> Result<Value> {
    use tokio::io::AsyncReadExt;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        run_id: String,
    }
    let request: Request = serde_json::from_value(params)?;
    ensure!(valid_run_id(&request.run_id), "Invalid run identity");
    let root = super::state_dir();
    let manual = super::manual::directory();
    let entry = tokio::task::spawn_blocking(move || -> Result<Entry> {
        load_catalog(&root, manual.as_deref())
            .entries
            .into_iter()
            .find(|e| e.record.job.id == request.run_id)
            .context("Run is no longer retained")
    })
    .await??;
    let job = entry.record.job;
    if let Some(path) = entry.manual_log {
        let (text, truncated) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::task::spawn_blocking(move || super::manual::read_log(&path)),
        )
        .await
        .context("Manual rebuild log read timed out")???;
        return Ok(json!({"log":{"run_id":job.id,"text":text,"truncated":truncated}}));
    }
    ensure!(
        !job.boot_id.is_empty()
            && job.boot_id.len() <= 40
            && job
                .boot_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'-'),
        "Invalid boot identity"
    );
    let read = async {
        let mut command = tokio::process::Command::new("journalctl");
        command
            .args(["--no-pager", "--quiet", "-n", "100", "--output=short-iso"])
            .arg(format!("_PID={}", job.pid))
            .arg(format!("_BOOT_ID={}", job.boot_id))
            .arg(format!("--since=@{}", job.started_at))
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if let Some(finished) = job.finished_at {
            command.arg(format!("--until=@{}", finished.saturating_add(1)));
        }
        let mut child = command.spawn().context("Open worker journal")?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("Journal output missing")?
            .take(32769)
            .read_to_end(&mut bytes)
            .await?;
        let truncated = bytes.len() > 32768;
        if truncated {
            child.kill().await?;
            bytes.truncate(32768);
        }
        let status = child.wait().await?;
        ensure!(
            status.success() || truncated,
            "Worker journal unavailable or not permitted"
        );
        Ok(
            json!({"log": {"run_id": job.id, "text": String::from_utf8_lossy(&bytes), "truncated": truncated}}),
        )
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), read)
        .await
        .context("Worker journal read timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_queries_and_stale_cursors_without_effects() {
        for params in [
            json!({"query":"x".repeat(513)}),
            json!({"limit":101}),
            json!({"run_id":"../outside"}),
            json!({"cursor":"old:0"}),
        ] {
            assert!(page(Catalog::default(), serde_json::from_value(params).unwrap()).is_err());
        }
        let empty = page(Catalog::default(), Query::default()).unwrap();
        assert_eq!(empty["history"]["available"], false);
        assert_eq!(empty["history"]["entries"], json!([]));
    }
    #[test]
    fn malformed_and_symlinked_files_are_partial_errors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("history")).unwrap();
        std::fs::write(root.path().join("history/bad.json"), "broken").unwrap();
        std::os::unix::fs::symlink("/missing", root.path().join("history/link.json")).unwrap();
        let catalog = load(root.path()).unwrap();
        assert!(catalog.available);
        assert_eq!(catalog.errors.len(), 2);
    }
}
