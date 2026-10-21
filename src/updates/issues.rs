//! Display-only recovery relationships over the retained catalog, never an acknowledgement.
use super::history::Entry;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Serialize)]
pub(crate) struct RunIssue {
    pub component: Option<usize>,
    pub failed_at: u64,
    pub recovered_at: Option<u64>,
    pub recovered_run_id: Option<String>,
}
#[derive(Clone, Serialize)]
pub(crate) struct OpenIssue {
    pub run_id: String,
    pub lane: String,
    pub component: String,
    pub incident: Option<String>,
    pub error: String,
    pub failed_at: u64,
}
fn component_key(entry: &Entry, name: &str) -> String {
    // AI updates and freshness checks observe the same tools; unrelated system
    // components and run-level follow-up failures must not clear them.
    format!(
        "{}:component:{name}",
        if entry.record.lane == "ai-tools-stale" {
            "ai-tools"
        } else {
            &entry.record.lane
        }
    )
}
fn job_key(entry: &Entry) -> String {
    format!(
        "{}:operation:{}",
        entry.record.lane, entry.record.job.operation
    )
}
pub(super) fn annotate(entries: &mut [Entry]) -> Vec<OpenIssue> {
    for entry in entries.iter_mut() {
        entry.issues.clear();
    }
    let mut order: Vec<_> = (0..entries.len()).collect();
    order.sort_by_key(|&index| {
        let job = &entries[index].record.job;
        (
            job.finished_at.unwrap_or(job.started_at),
            job.started_at,
            job.id.clone(),
        )
    });
    let mut pending: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for index in order {
        let entry = &entries[index];
        if entry.outcome == "running" || entry.record.job.exit_code == Some(130) {
            continue;
        }
        let at = entry
            .record
            .job
            .finished_at
            .unwrap_or(entry.record.job.started_at);
        let id = entry.record.job.id.clone();
        let mut failures = Vec::new();
        let mut recoveries = Vec::new();
        for (part_index, part) in entry.record.components.iter().enumerate() {
            if part.phase == "held" {
                continue;
            } // A deliberate hold is not recovery.
            let key = component_key(entry, &part.name);
            if part.error.as_ref().is_some_and(|error| !error.is_empty()) || part.phase == "failed"
            {
                failures.push((key, Some(part_index)));
            } else if entry.outcome != "interrupted"
                && part.incident.is_none()
                && ["current", "activated"].contains(&part.phase.as_str())
            {
                recoveries.push(key);
            }
        }
        if failures.is_empty()
            && ["failed", "partial", "blocked", "interrupted"].contains(&entry.outcome.as_str())
        {
            failures.push((job_key(entry), None));
        }
        if entry.record.job.error.is_none()
            && !entry
                .record
                .components
                .iter()
                .any(|part| part.phase == "held" || part.incident.is_some())
            && [
                "current",
                "activated",
                "staged",
                "ready",
                "approved",
                "completed",
            ]
            .contains(&entry.outcome.as_str())
        {
            recoveries.push(job_key(entry));
        }
        for key in recoveries {
            if let Some(previous) = pending.remove(&key) {
                for (run, issue_index) in previous {
                    let issue = &mut entries[run].issues[issue_index];
                    // Equal timestamps do not establish ordering between distinct runs.
                    if at > issue.failed_at {
                        issue.recovered_at = Some(at);
                        issue.recovered_run_id = Some(id.clone());
                    } else {
                        pending
                            .entry(key.clone())
                            .or_default()
                            .push((run, issue_index));
                    }
                }
            }
        }
        for (key, component) in failures {
            let issue_index = entries[index].issues.len();
            entries[index].issues.push(RunIssue {
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
            let issue = &entry.issues[issue];
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
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(id: &str, at: u64, name: &str, error: Option<&str>) -> Entry {
        let record = serde_json::from_value(serde_json::json!({
            "schema_version":1,"lane":"ai-tools","notification_owner":"desktop",
            "components":[{"name":name,"before":"1","after":"1","changed":false,
                "phase":if error.is_some(){"failed"}else{"current"},"error":error,"incident":error}],
            "job":{"schema_version":1,"id":id,"operation":"ai-update","status":if error.is_some(){"failed"}else{"completed"},
                "phase":"current","started_at":at,"updated_at":at,"finished_at":at,"pid":1,"boot_id":"boot","process_start":"1","exit_code":0,"error":null}
        })).unwrap();
        Entry {
            record,
            outcome: if error.is_some() { "failed" } else { "current" }.into(),
            title: "Run".into(),
            failed_stage: None,
            deployed: None,
            issues: vec![],
            manual_log: None,
        }
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
    fn outcome_codes_are_searchable_independently_of_title_and_phase() {
        let mut blocked = run("opaque", 1, "pi", Some("bad"));
        blocked.outcome = "blocked".into();
        let catalog = super::super::history::Catalog {
            available: true,
            entries: vec![blocked],
            ..Default::default()
        };
        let page = super::super::history::page(
            catalog,
            super::super::history::Query {
                query: "BLOCKED".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page["history"]["total"], 1);
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
