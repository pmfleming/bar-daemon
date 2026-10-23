//! Bounded, read-only catalog of producer-owned run records. No deployment effects.
use super::catalog::{Catalog, Entry};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use shelllist_update_status::{
    HISTORY_LIMIT, HISTORY_MAX_BYTES, JobState, RunRecord, valid_run_id,
};
use std::{
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::Path,
};

mod search;

#[derive(Debug)]
pub(crate) struct StaleCursor;
impl std::fmt::Display for StaleCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Update history changed; refresh before continuing")
    }
}
impl std::error::Error for StaleCursor {}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Query {
    pub query: String,
    pub cursor: Option<String>,
    pub run_id: Option<String>,
    pub limit: Option<usize>,
    pub lane: Option<String>,
    pub outcome: Option<String>,
    pub anchor_ids: Vec<String>,
}
fn load(root: &Path) -> Result<Catalog> {
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
    let cutoff = (crate::time::unix_ms() / 1000)
        .saturating_sub(shelllist_update_status::HISTORY_DAYS * 86400);
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
        match read_entry(&path, owner) {
            Ok(entry) if entry.retained_since(cutoff) => result.entries.push(entry),
            Ok(_) => {}
            Err(error) => result.record_error(
                path.file_name().unwrap_or_default().to_string_lossy(),
                error,
            ),
        }
    }
    Ok(result)
}

fn read_entry(path: &Path, owner: u32) -> Result<Entry> {
    let record = RunRecord::decode(&super::files::read(path, owner, HISTORY_MAX_BYTES)?)?;
    ensure!(
        path.file_stem().and_then(|name| name.to_str()) == Some(&record.job.id),
        "History identity mismatch"
    );
    let interrupted =
        record.job.status == JobState::Running && !super::jobs::is_running(&record.job);
    Ok(Entry {
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
    })
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
    // Sort once, after merging both sources.
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
    catalog.open_issues = super::catalog::annotate(&mut catalog.entries);
    catalog
}

pub(crate) async fn query(params: Value) -> Result<Value> {
    let query: Query = serde_json::from_value(params)?;
    let directory = super::state_dir();
    let manual = super::manual::directory();
    let read = super::reader::run(move || page(load_catalog(&directory, manual.as_deref()), query));
    tokio::time::timeout(std::time::Duration::from_secs(3), read)
        .await
        .context("Update history read timed out")?
}

impl Query {
    fn validate(&self) -> Result<usize> {
        let limit = self.limit.unwrap_or(50);
        ensure!(
            self.query.len() <= 512 && (1..=100).contains(&limit),
            "Invalid update history query"
        );
        ensure!(
            self.run_id.as_ref().is_none_or(|id| valid_run_id(id)),
            "Invalid run identity"
        );
        ensure!(
            self.lane
                .as_deref()
                .is_none_or(|lane| ["system", "ai-tools", "ai-tools-stale"].contains(&lane)),
            "Invalid history lane filter"
        );
        ensure!(
            self.outcome.as_deref().is_none_or(|outcome| [
                "problems",
                "running",
                "failed",
                "partial",
                "blocked",
                "interrupted",
                "activated",
                "staged",
                "ready",
                "queued",
                "current",
                "available",
                "skipped",
                "approved",
                "completed"
            ]
            .contains(&outcome)),
            "Invalid history outcome filter"
        );
        ensure!(
            self.anchor_ids.len() <= 2 && self.anchor_ids.iter().all(|id| valid_run_id(id)),
            "Invalid history anchors"
        );
        Ok(limit)
    }

    fn offset(&self, revision: &str) -> Result<usize> {
        let Some(cursor) = &self.cursor else {
            return Ok(0);
        };
        ensure!(cursor.len() < 100, "Invalid history cursor");
        let (rev, offset) = cursor.split_once(':').context("Invalid history cursor")?;
        ensure!(rev == revision, StaleCursor);
        Ok(offset.parse()?)
    }

    fn matches(
        &self,
        entry: &Entry,
        search: &search::Search,
        now: &chrono::DateTime<chrono::Local>,
    ) -> bool {
        // A selected run intentionally overrides the otherwise-valid filters.
        if let Some(id) = &self.run_id {
            return &entry.record.job.id == id;
        }
        let outcome_matches = self.outcome.as_deref().is_none_or(|outcome| match outcome {
            "problems" => entry.is_problem(),
            outcome => outcome == entry.outcome,
        });
        self.lane
            .as_ref()
            .is_none_or(|lane| lane == &entry.record.lane)
            && outcome_matches
            && search.matches_with(entry.record.job.started_at, &now.timezone(), || {
                format!(
                    "{} {} {} {} {} {} {}",
                    entry.outcome,
                    entry.title,
                    entry.record.lane,
                    entry.record.job.operation,
                    entry.record.job.id,
                    entry.record.job.error.as_deref().unwrap_or_default(),
                    serde_json::to_string(&entry.record.components).unwrap_or_default()
                )
                .to_lowercase()
            })
    }
}

// Always admit the first row, even if it exceeds the byte budget: cursors must advance.
fn page_end(entries: &[Entry], offset: usize, limit: usize) -> Result<usize> {
    ensure!(offset <= entries.len(), "Invalid history offset");
    let mut end = offset;
    let mut bytes = 0;
    for entry in entries.iter().skip(offset).take(limit) {
        let size = super::json::Measure::of(entry)?.bytes;
        if end > offset && bytes + size > 128 * 1024 {
            break;
        }
        bytes += size;
        end += 1;
    }
    Ok(end)
}

pub(crate) fn page(catalog: Catalog, query: Query) -> Result<Value> {
    page_at(catalog, query, chrono::Local::now())
}

fn page_at(catalog: Catalog, query: Query, now: chrono::DateTime<chrono::Local>) -> Result<Value> {
    let limit = query.validate()?;
    let search = search::Search::parse(&query.query, &now)?;
    let mut hasher = super::json::Measure::of(&catalog.entries)?.hash;
    query.query.hash(&mut hasher);
    query.run_id.hash(&mut hasher);
    query.lane.hash(&mut hasher);
    query.outcome.hash(&mut hasher);
    query.anchor_ids.hash(&mut hasher);
    search.hash(&mut hasher); // Relative dates fence cursors across local midnight.
    let revision = format!("{:016x}", hasher.finish());
    let offset = query.offset(&revision)?;
    let matching: Vec<_> = catalog
        .entries
        .into_iter()
        .filter(|entry| query.matches(entry, &search, &now))
        .collect();
    let retain_through = matching
        .iter()
        .enumerate()
        .filter(|(_, entry)| query.anchor_ids.contains(&entry.record.job.id))
        .map(|(index, _)| index + 1)
        .max()
        .unwrap_or(0);
    let end = page_end(&matching, offset, limit)?;
    Ok(json!({"history": {
        "available": catalog.available, "revision": revision, "total": matching.len(), "retain_through": retain_through,
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
    let entry = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        super::reader::run(move || {
            load_catalog(&root, manual.as_deref())
                .entries
                .into_iter()
                .find(|e| e.record.job.id == request.run_id)
                .context("Run is no longer retained")
        }),
    )
    .await
    .context("Update log lookup timed out")??;
    let job = entry.record.job;
    if let Some(path) = entry.manual_log {
        let (text, truncated) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            super::reader::run(move || super::manual::read_log(&path)),
        )
        .await
        .context("Manual rebuild log read timed out")??;
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
    use super::{Catalog, Entry, Query, StaleCursor, load, page};
    use chrono::TimeZone;
    use serde_json::json;

    fn catalog() -> Catalog {
        let outcomes = [
            "failed",
            "current",
            "partial",
            "staged",
            "blocked",
            "current",
            "running",
            "interrupted",
        ];
        let entries = outcomes.iter().enumerate().rev().map(|(index, outcome)| {
            let at = chrono::Local.with_ymd_and_hms(2026, 10, 20 + (index / 4) as u32, 12, index as u32, 0).unwrap().timestamp();
            Entry {
                record: serde_json::from_value(json!({"schema_version":1,"lane":if index % 2 == 0 {"system"} else {"ai-tools"},
                    "notification_owner":"desktop","components":[],
                    "job":{"schema_version":1,"id":format!("run-{index}"),"operation":"check-delayed","status":"completed","phase":"current",
                        "started_at":at,"updated_at":at+1,"finished_at":at+1,"pid":1,"boot_id":"boot","process_start":"1","exit_code":0,"error":null}})).unwrap(),
                outcome: (*outcome).into(), title: "Opaque title".into(), failed_stage: None, deployed: None, issues: vec![], manual_log: None,
            }
        }).collect();
        Catalog {
            available: true,
            entries,
            ..Default::default()
        }
    }
    #[test]
    #[ignore = "manual default-history performance probe; setup excluded"]
    fn profile_default_history_pages() {
        use std::{hint::black_box, time::Instant};
        let catalogs: Vec<_> = (0..8)
            .map(|_| {
                let entries = (0..1000)
                    .map(|index| {
                        let mut entry = super::super::catalog::test_entry(
                            &format!("run-{index}"),
                            1_790_000_000,
                            "test",
                            None,
                        );
                        entry.record.components = (0..8)
                            .map(|n| shelllist_update_status::ComponentResult {
                                name: format!("component-{n}"),
                                phase: "current".into(),
                                before: None,
                                after: None,
                                changed: false,
                                error: Some("retained diagnostic context ".repeat(12)),
                                incident: None,
                            })
                            .collect();
                        entry
                    })
                    .collect();
                Catalog {
                    available: true,
                    entries,
                    ..Default::default()
                }
            })
            .collect();
        let start = Instant::now();
        for catalog in catalogs {
            black_box(page(catalog, Query::default()).unwrap());
        }
        println!(
            "8 default pages / 1000 rows with 8 components: {:.3} ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn filters_dates_and_retention_anchors_apply_before_pagination() {
        for (params, total) in [
            (json!({"lane":"system","outcome":"problems"}), 3),
            (
                json!({"lane":"system","outcome":"problems","query":"2026-10-21"}),
                1,
            ),
            (json!({"outcome":"current"}), 2),
            // Outcome codes are searchable independently of the opaque title/phase.
            (json!({"query":"BLOCKED"}), 1),
            (json!({"query":"since:2026-10-20 before:2026-10-21"}), 4),
            (json!({"query":"opaque 2026-10-21"}), 4),
            (
                json!({"run_id":"run-1","lane":"system","outcome":"problems","query":"missing"}),
                1,
            ),
        ] {
            let result = page(catalog(), serde_json::from_value(params.clone()).unwrap()).unwrap();
            assert_eq!(result["history"]["total"], total, "{params}");
        }
        let first = page(
            catalog(),
            serde_json::from_value(json!({"limit":2,"anchor_ids":["run-1","missing"]})).unwrap(),
        )
        .unwrap();
        assert_eq!(first["history"]["retain_through"], 7);
        let cursor = first["history"]["cursor"].as_str().unwrap();
        let second = page(
            catalog(),
            serde_json::from_value(
                json!({"limit":2,"anchor_ids":["run-1","missing"],"cursor":cursor}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(second["history"]["entries"][0]["job"]["id"], "run-5");
        assert_eq!(second["history"]["retain_through"], 7);
        let error = page(
            catalog(),
            serde_json::from_value(
                json!({"limit":2,"anchor_ids":["run-1","missing"],"cursor":cursor,"lane":"system"}),
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(error.is::<StaleCursor>());
        let mut changed = catalog();
        changed.entries.remove(0);
        assert!(
            page(
                changed,
                serde_json::from_value(json!({"cursor":cursor,"anchor_ids":["run-1","missing"]}))
                    .unwrap()
            )
            .unwrap_err()
            .is::<StaleCursor>()
        );
        for params in [
            json!({"lane":"unknown"}),
            json!({"outcome":"unknown"}),
            json!({"query":"on:2025-02-29"}),
            json!({"anchor_ids":["a","b","c"]}),
            json!({"anchor_ids":["../bad"]}),
        ] {
            assert!(page(catalog(), serde_json::from_value(params).unwrap()).is_err());
        }
    }
    #[test]
    fn filtering_failures_does_not_hide_their_later_recovery() {
        let mut catalog = catalog();
        catalog
            .entries
            .retain(|entry| ["run-0", "run-1"].contains(&entry.record.job.id.as_str()));
        for entry in &mut catalog.entries {
            entry.record.lane = "ai-tools".into();
            let failed = entry.outcome == "failed";
            entry.record.components = vec![shelllist_update_status::ComponentResult {
                name: "pi".into(),
                phase: if failed { "failed" } else { "current" }.into(),
                before: None,
                after: None,
                changed: false,
                error: failed.then(|| "build failed".into()),
                incident: None,
            }];
        }
        super::super::catalog::annotate(&mut catalog.entries);
        let result = page(
            catalog,
            serde_json::from_value(json!({"outcome":"problems"})).unwrap(),
        )
        .unwrap();
        assert_eq!(result["history"]["total"], 1);
        assert_eq!(
            result["history"]["entries"][0]["issues"][0]["recovered_run_id"],
            "run-1"
        );
    }
    #[test]
    fn byte_limited_pages_make_progress_even_for_one_oversized_entry() {
        for title_bytes in [70_000, 140_000] {
            let mut catalog = catalog();
            for entry in &mut catalog.entries {
                entry.title = "x".repeat(title_bytes);
            }
            let page = page(catalog, Query::default()).unwrap();
            assert_eq!(page["history"]["entries"].as_array().unwrap().len(), 1);
            assert!(page["history"]["cursor"].as_str().unwrap().ends_with(":1"));
        }
    }

    #[test]
    fn rejects_bad_queries_and_stale_cursors_without_effects() {
        for params in [
            json!({"query":"x".repeat(513)}),
            json!({"limit":101}),
            json!({"limit":0}),
            json!({"run_id":"run-1", "lane":"invalid"}),
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
    fn cursor_boundaries_and_relative_dates_are_fenced() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 20, 12, 0, 0)
            .unwrap();
        let query = || Query {
            query: "today".into(),
            ..Default::default()
        };
        let first = super::page_at(Catalog::default(), query(), now).unwrap();
        let revision = first["history"]["revision"].as_str().unwrap();
        for offset in ["-1", "overflow", "1", "184467440737095516160"] {
            let query = Query {
                cursor: Some(format!("{revision}:{offset}")),
                ..query()
            };
            assert!(super::page_at(Catalog::default(), query, now).is_err());
        }
        let query = Query {
            cursor: Some(format!("{revision}:0")),
            ..query()
        };
        assert!(
            super::page_at(Catalog::default(), query, now + chrono::Duration::days(1))
                .unwrap_err()
                .is::<StaleCursor>()
        );
        assert_eq!(super::page_end(&[], 0, 50).unwrap(), 0);
        assert!(super::page_end(&[], 1, 50).is_err());
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
        let record =
            super::super::catalog::test_entry("valid", crate::time::unix_ms() / 1000, "pi", None)
                .record;
        std::fs::write(
            root.path().join("history/valid.json"),
            record.encode().unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join("history/mismatched.json"),
            record.encode().unwrap(),
        )
        .unwrap();
        for index in 0..20 {
            std::fs::write(
                root.path().join(format!("history/bad-{index}.json")),
                "broken",
            )
            .unwrap();
        }
        let catalog = load(root.path()).unwrap();
        assert_eq!(catalog.errors.len(), 8);
        assert_eq!(
            catalog.entries.len(),
            1,
            "bad records must not hide valid records"
        );
        assert_eq!(catalog.entries[0].record.job.id, "valid");
    }
}
