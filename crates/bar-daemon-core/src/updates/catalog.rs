//! Shared read-only catalog and display-only recovery relationships. No I/O or acknowledgements.
use serde::Serialize;
use shelllist_update_status::RunRecord;
use std::{collections::BTreeMap, hash::Hasher, path::PathBuf};

#[derive(Serialize)]
pub(crate) struct Entry {
    #[serde(flatten)]
    pub record: RunRecord,
    pub outcome: String,
    pub title: String,
    pub failed_stage: Option<String>,
    pub deployed: Option<bool>,
    pub issues: Vec<RunIssue>,
    #[serde(skip)]
    pub manual_log: Option<PathBuf>,
}
impl Entry {
    pub(super) fn is_problem(&self) -> bool {
        matches!(
            self.outcome.as_str(),
            "failed" | "partial" | "blocked" | "interrupted"
        )
    }

    pub(super) fn is_manual(&self) -> bool {
        self.record.job.operation == "manual-rebuild"
    }

    pub(super) fn retained_since(&self, cutoff: u64) -> bool {
        self.outcome == "running"
            || self
                .record
                .job
                .finished_at
                .unwrap_or(self.record.job.updated_at)
                >= cutoff
    }

    fn finished_at(&self) -> u64 {
        self.record
            .job
            .finished_at
            .unwrap_or(self.record.job.started_at)
    }
}

#[derive(Default)]
pub(crate) struct Catalog {
    pub entries: Vec<Entry>,
    pub available: bool,
    pub errors: Vec<String>,
    pub open_issues: Vec<OpenIssue>,
}
impl Catalog {
    pub(super) fn revision(&self) -> serde_json::Result<u64> {
        Ok(
            super::json::Measure::of(&(&self.entries, &self.errors, self.available))?
                .hash
                .finish(),
        )
    }

    pub(super) fn record_error(&mut self, source: impl std::fmt::Display, error: anyhow::Error) {
        if self.errors.len() < 8 {
            self.errors.push(format!("{source}: {error:#}"));
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RunIssue {
    pub component: Option<usize>,
    pub failed_at: u64,
    pub recovered_at: Option<u64>,
    pub recovered_run_id: Option<String>,
}
impl RunIssue {
    fn recover(&mut self, at: u64, run_id: &str) -> bool {
        // Equal timestamps do not establish ordering between distinct runs.
        if at <= self.failed_at {
            return false;
        }
        self.recovered_at = Some(at);
        self.recovered_run_id = Some(run_id.into());
        true
    }
}

#[derive(Serialize)]
pub(crate) struct OpenIssue {
    pub run_id: String,
    pub lane: String,
    pub component: String,
    pub incident: Option<String>,
    pub error: String,
    pub failed_at: u64,
}

// Borrow catalog identity; recovery indexing needs neither formatted keys nor cloned records.
type IssueKey<'a> = (&'a str, &'static str, &'a str);
fn component_key<'a>(entry: &'a Entry, name: &'a str) -> IssueKey<'a> {
    // AI updates and freshness checks observe the same tools; unrelated system
    // components and run-level follow-up failures must not clear them.
    let lane = if entry.record.lane == "ai-tools-stale" {
        "ai-tools"
    } else {
        &entry.record.lane
    };
    (lane, "component", name)
}
fn job_key(entry: &Entry) -> IssueKey<'_> {
    (&entry.record.lane, "operation", &entry.record.job.operation)
}
pub(super) fn annotate(entries: &mut [Entry]) -> Vec<OpenIssue> {
    // Keep identity immutable while indexing borrowed keys; publish annotations together.
    let mut issues: Vec<_> = entries
        .iter_mut()
        .map(|entry| {
            entry.issues.clear();
            std::mem::take(&mut entry.issues)
        })
        .collect();
    let mut order: Vec<_> = (0..entries.len()).collect();
    order.sort_by_key(|&index| {
        let entry = &entries[index];
        (
            entry.finished_at(),
            entry.record.job.started_at,
            &entry.record.job.id,
        )
    });
    let mut pending: BTreeMap<IssueKey<'_>, Vec<(usize, usize)>> = BTreeMap::new();
    for index in order {
        let entry = &entries[index];
        if entry.outcome == "running" || entry.record.job.exit_code == Some(130) {
            continue;
        }
        let at = entry.finished_at();
        let changes = Changes::for_entry(entry);
        for key in changes.recoveries {
            if let Some(previous) = pending.get_mut(&key) {
                previous
                    .retain(|&(run, issue)| !issues[run][issue].recover(at, &entry.record.job.id));
            }
        }
        for (key, component) in changes.failures {
            let issue_index = issues[index].len();
            issues[index].push(RunIssue {
                component,
                failed_at: at,
                recovered_at: None,
                recovered_run_id: None,
            });
            pending.entry(key).or_default().push((index, issue_index));
        }
    }
    let mut open: Vec<_> = pending
        .values()
        .filter_map(|runs| runs.last())
        .map(|&(index, issue)| {
            let entry = &entries[index];
            let issue = &issues[index][issue];
            let part = issue
                .component
                .and_then(|index| entry.record.components.get(index));
            OpenIssue {
                run_id: entry.record.job.id.clone(),
                lane: entry.record.lane.clone(),
                component: part.map_or_else(
                    || entry.record.job.operation.chars().take(128).collect(),
                    |part| part.name.clone(),
                ),
                incident: part.and_then(|part| part.incident.clone()),
                error: part
                    .and_then(|part| part.error.as_deref())
                    .or(entry.record.job.error.as_deref())
                    .unwrap_or(&entry.title)
                    .chars()
                    .take(256)
                    .collect(),
                failed_at: issue.failed_at,
            }
        })
        .collect();
    open.sort_by(|a, b| {
        (b.failed_at, &b.run_id, &b.component).cmp(&(a.failed_at, &a.run_id, &a.component))
    });
    for (entry, issues) in entries.iter_mut().zip(issues) {
        entry.issues = issues;
    }
    open
}

#[derive(Default)]
struct Changes<'a> {
    failures: Vec<(IssueKey<'a>, Option<usize>)>,
    recoveries: Vec<IssueKey<'a>>,
}
impl<'a> Changes<'a> {
    fn for_entry(entry: &'a Entry) -> Self {
        let mut changes = Self::default();
        for (index, part) in entry
            .record
            .components
            .iter()
            .enumerate()
            .filter(|(_, part)| part.phase != "held")
        {
            let key = component_key(entry, &part.name);
            if part.error.as_ref().is_some_and(|error| !error.is_empty()) || part.phase == "failed"
            {
                changes.failures.push((key, Some(index)));
            } else if entry.outcome != "interrupted"
                && part.incident.is_none()
                && matches!(part.phase.as_str(), "current" | "activated")
            {
                changes.recoveries.push(key);
            }
        }
        if changes.failures.is_empty() && entry.is_problem() {
            changes.failures.push((job_key(entry), None));
        }
        if entry.record.job.error.is_none()
            && !entry
                .record
                .components
                .iter()
                .any(|part| part.phase == "held" || part.incident.is_some())
            && matches!(
                entry.outcome.as_str(),
                "current" | "activated" | "staged" | "ready" | "approved" | "completed"
            )
        {
            changes.recoveries.push(job_key(entry));
        }
        changes
    }
}

#[cfg(test)]
pub(super) fn test_entry(id: &str, at: u64, name: &str, error: Option<&str>) -> Entry {
    let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"lane":"ai-tools","notification_owner":"desktop",
            "components":[{"name":name,"before":"1","after":"1","changed":false,
                "phase":if error.is_some(){"failed"}else{"current"},"error":error,"incident":error}],
            "job":{"schema_version":1,"id":id,"operation":"ai-update","status":if error.is_some(){"failed"}else{"completed"},
                "phase":"current","started_at":at,"updated_at":at,"finished_at":at,"pid":1,"boot_id":"boot","process_start":"1","exit_code":0,"error":null}
        })).unwrap();
    Entry {
        outcome: record.outcome().into(),
        title: record.title().into(),
        record,
        failed_stage: None,
        deployed: None,
        issues: vec![],
        manual_log: None,
    }
}
#[cfg(test)]
mod tests {
    use super::{Catalog, annotate, test_entry as run};

    #[test]
    fn revision_tracks_records_errors_and_availability_but_is_repeatable() {
        let mut catalog = Catalog::default();
        let mut previous = catalog.revision().unwrap();
        assert_eq!(catalog.revision().unwrap(), previous);
        catalog.available = true;
        let next = catalog.revision().unwrap();
        assert_ne!(next, previous);
        previous = next;
        catalog.errors.push("unreadable record".into());
        let next = catalog.revision().unwrap();
        assert_ne!(next, previous);
        previous = next;
        catalog.entries.push(run("run", 10, "pi", None));
        assert_ne!(catalog.revision().unwrap(), previous);
    }

    #[test]
    fn retention_preserves_live_runs_and_the_cutoff_boundary() {
        let mut entry = run("run", 10, "pi", None);
        assert!(entry.retained_since(10));
        assert!(!entry.retained_since(11));
        entry.outcome = "running".into();
        assert!(entry.retained_since(11));
        entry.outcome = "interrupted".into();
        entry.record.job.finished_at = None;
        entry.record.job.updated_at = 12;
        assert!(entry.retained_since(12));
        assert!(!entry.retained_since(13));
    }

    #[test]
    fn latest_failures_recover_independently_and_recurrences_remain_open() {
        let mut entries = vec![
            run("first", 1, "pi", Some("build-a")),
            run("other", 2, "codex", None),
            run("repeat", 3, "pi", Some("build-a")),
            run("recovered", 4, "pi", None),
            run("again", 5, "pi", Some("build-b")),
        ];
        let open = annotate(&mut entries);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].run_id, "again");
        assert_eq!(entries[0].issues[0].recovered_at, Some(4));
        assert_eq!(
            entries[2].issues[0].recovered_run_id.as_deref(),
            Some("recovered")
        );
        assert!(entries[4].issues[0].recovered_at.is_none());
        entries.reverse();
        assert_eq!(
            annotate(&mut entries)[0].run_id,
            "again",
            "input/search order is irrelevant"
        );
    }
    #[test]
    fn holds_do_not_clear_job_failures_or_hide_unrelated_job_errors() {
        let mut failed = run("failed", 1, "pi", Some("job failure"));
        failed.record.components.clear();
        let mut held = run("held", 2, "pi", None);
        held.record.components[0].phase = "held".into();
        let mut entries = vec![failed, held];
        assert_eq!(annotate(&mut entries)[0].run_id, "failed");
        entries[1].outcome = "failed".into();
        entries[1].record.job.error = Some("Unrelated job failure".into());
        assert_eq!(annotate(&mut entries)[0].run_id, "held");
        assert!(entries[0].issues[0].recovered_at.is_none());
    }
    #[test]
    fn recovery_keys_separate_scope_and_lane_and_annotations_are_repeatable() {
        for (lane, job_only, recovered) in [
            ("ai-tools", true, false),
            ("system", false, false),
            ("ai-tools-stale", false, true),
        ] {
            let mut next = run("next", 2, "pi", None);
            next.record.lane = lane.into();
            if job_only {
                next.record.components.clear();
            }
            let mut entries = vec![run("failed", 1, "pi", Some("build")), next];
            let open = annotate(&mut entries);
            assert_eq!(open.is_empty(), recovered, "{lane}/{job_only}");
            assert_eq!(entries[0].issues[0].recovered_at.is_some(), recovered);
            let first = serde_json::to_value(&entries).unwrap();
            annotate(&mut entries);
            assert_eq!(serde_json::to_value(&entries).unwrap(), first);
        }
    }

    #[test]
    fn running_held_cancelled_and_equal_time_results_do_not_invent_recovery() {
        for variant in ["running", "interrupted", "held", "cancelled", "same-time"] {
            let mut newer = run("newer", 2, "pi", None);
            match variant {
                "running" | "interrupted" => newer.outcome = variant.into(),
                "held" => newer.record.components[0].phase = "held".into(),
                "cancelled" => newer.record.job.exit_code = Some(130),
                _ => newer.record.job.finished_at = Some(1),
            }
            let mut entries = vec![run("failed", 1, "pi", Some("build")), newer];
            let open = annotate(&mut entries);
            assert_eq!(
                open.len(),
                if variant == "interrupted" { 2 } else { 1 },
                "{variant}"
            );
            assert!(open.iter().any(|issue| issue.run_id == "failed"));
            assert!(entries[0].issues[0].recovered_at.is_none());
        }
    }
}
