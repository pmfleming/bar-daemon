//! One session-owned publisher. Notification failures never affect update jobs.
use super::history::{Catalog, Entry};
use crate::activity::notifications::{
    model::{IncomingNotification, NotificationAction, NotificationHints},
    service::NotificationService,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use shelllist_update_status::NotificationOwner;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

#[derive(Default, Clone, Serialize, Deserialize)]
struct Delivery {
    initialized: bool,
    #[serde(default)]
    manual_initialized: bool,
    seen: BTreeSet<String>,
    incidents: BTreeMap<String, String>,
}

pub(super) struct Publisher {
    service: Arc<NotificationService>,
    path: PathBuf,
    first_scan: bool,
}
impl Publisher {
    pub(super) fn new(service: Arc<NotificationService>) -> Self {
        Self {
            service,
            path: crate::activity::config::notification_database_path()
                .with_file_name("update-delivery.json"),
            first_scan: true,
        }
    }
    pub(super) async fn reconcile(&mut self, catalog: &Catalog) -> Result<()> {
        if !catalog.available || !catalog.errors.is_empty() {
            return Ok(());
        }
        let path = self.path.clone();
        let mut state: Delivery = tokio::task::spawn_blocking(move || -> Result<Delivery> {
            Ok(
                shelllist_daemon_core::read_bytes_bounded(&path, 512 * 1024)?
                    .map(|bytes| serde_json::from_slice(&bytes))
                    .transpose()?
                    .unwrap_or_default(),
            )
        })
        .await??;
        let old = serde_json::to_vec(&state)?;
        let initial = !state.initialized;
        let manual_initial = !state.manual_initialized;
        let mut events = Vec::new();
        // Process chronologically; a later recovery can retire an earlier issue.
        for entry in catalog.entries.iter().rev().filter(|e| {
            e.outcome != "running" && e.record.notification_owner == NotificationOwner::Desktop
        }) {
            if state.seen.contains(&entry.record.job.id) {
                continue;
            }
            let new_problem = update_incidents(&mut state, entry);
            let initial_source = if entry.record.job.operation == "manual-rebuild" {
                manual_initial
            } else {
                initial
            };
            if new_problem
                || (!initial_source
                    && (matches!(entry.outcome.as_str(), "activated" | "staged" | "partial")
                        || (entry.record.job.operation == "manual-rebuild"
                            && matches!(entry.outcome.as_str(), "current" | "completed"))))
            {
                events.push(entry);
            }
            state.seen.insert(entry.record.job.id.clone());
        }
        state.initialized = true;
        state.manual_initialized = true;
        // Retention follows the producer's bounded catalog, not an unbounded outbox.
        state
            .seen
            .retain(|id| catalog.entries.iter().any(|e| &e.record.job.id == id));
        events.retain(|e| {
            let initial_source = if e.record.job.operation == "manual-rebuild" {
                manual_initial
            } else {
                initial
            };
            !initial_source
                || incident_pairs(e)
                    .iter()
                    .any(|(key, value)| state.incidents.get(key) == Some(value))
        });
        if let Some(last) = events.last() {
            let catchup = self.first_scan || events.len() > 1;
            let summary = if catchup {
                "Recent updates to review"
            } else {
                &last.title
            };
            let body = if catchup {
                format!(
                    "{} update outcome(s); {} unresolved issue(s). Open Updates to review.",
                    events.len(),
                    state.incidents.len()
                )
            } else if matches!(
                last.outcome.as_str(),
                "failed" | "blocked" | "partial" | "interrupted"
            ) {
                // No raw journal/error body in desktop popups.
                "Review the recorded outcome and diagnostics in Updates.".into()
            } else if last.outcome == "staged" {
                "The candidate is staged for the next boot, not activated in this session.".into()
            } else if last.record.job.operation == "manual-rebuild" && last.outcome == "current" {
                "The rebuild completed without changing the recorded system.".into()
            } else if last.record.job.operation == "manual-rebuild" && last.outcome == "completed" {
                "Rebuild command completed. Review the recorded deployment status in Updates."
                    .into()
            } else if last.record.lane == "system" {
                "System activation completed. Review the recorded run for details.".into()
            } else {
                format!(
                    "{} component(s) changed. Review the run for details.",
                    last.record.components.iter().filter(|c| c.changed).count()
                )
            };
            let notification = IncomingNotification {
                app_name: "Shelllist Updates".into(),
                app_icon: "shelllist-updates".into(),
                summary: summary.into(),
                body,
                actions: if self.service.native_engine().is_some() {
                    vec![NotificationAction {
                        key: "default".into(),
                        label: "Review update".into(),
                    }]
                } else {
                    vec![]
                },
                hints: NotificationHints {
                    desktop_entry: "shelllist-updates".into(),
                    urgency: 1,
                    category: "system.update".into(),
                    suppress_sound: true,
                    ..Default::default()
                },
                expire_timeout: -1,
            };
            let target = if catchup { "" } else { &last.record.job.id };
            // Stable across a crash after delivery but before the cursor write.
            let key = format!("updates:run:{}", last.record.job.id);
            if let Some(engine) = self.service.native_engine() {
                engine
                    .notify_update_once(key, target.into(), notification)
                    .await?;
            } else {
                // External Notify has no durable idempotency key or retained action
                // owner. It gets a truthful summary and the desktop/tray fallback.
                self.service.sink().send(notification).await?;
            }
        }
        let bytes = serde_json::to_vec(&state)?;
        if bytes != old {
            let path = self.path.clone();
            tokio::task::spawn_blocking(move || {
                shelllist_daemon_core::write_bytes_atomic(
                    &path,
                    &bytes,
                    shelllist_daemon_core::AtomicFilePolicy::PRIVATE,
                )
            })
            .await
            .context("Join update delivery persistence")??;
        }
        self.first_scan = false;
        Ok(())
    }
}

fn incident_pairs(entry: &Entry) -> Vec<(String, String)> {
    if entry.outcome == "interrupted" && entry.record.job.exit_code == Some(130) {
        return Vec::new();
    }
    let mut pairs: Vec<_> = entry
        .record
        .components
        .iter()
        .filter(|c| c.phase != "held")
        .filter_map(|c| {
            c.incident
                .as_ref()
                .map(|incident| (format!("tool:{}", c.name), incident.clone()))
        })
        .collect();
    if pairs.is_empty()
        && !entry.record.components.iter().any(|c| c.phase == "held")
        && matches!(
            entry.outcome.as_str(),
            "failed" | "blocked" | "interrupted" | "partial"
        )
    {
        pairs.push((
            format!("{}:{}", entry.record.lane, entry.record.job.operation),
            entry
                .record
                .job
                .error
                .as_deref()
                .unwrap_or(&entry.outcome)
                .lines()
                .next()
                .unwrap_or("unknown")
                .chars()
                .take(256)
                .collect(),
        ));
    }
    pairs
}
fn update_incidents(state: &mut Delivery, entry: &Entry) -> bool {
    let pairs = incident_pairs(entry);
    let fresh = pairs
        .iter()
        .any(|(key, value)| state.incidents.get(key) != Some(value));
    for component in &entry.record.components {
        if component.phase == "held"
            || (component.incident.is_none()
                && ["current", "activated"].contains(&component.phase.as_str()))
        {
            state.incidents.remove(&format!("tool:{}", component.name));
        }
    }
    if pairs.is_empty()
        && matches!(
            entry.outcome.as_str(),
            "current" | "activated" | "staged" | "ready" | "approved"
        )
    {
        state.incidents.remove(&format!(
            "{}:{}",
            entry.record.lane, entry.record.job.operation
        ));
    }
    state.incidents.extend(pairs);
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn manual_rebuilds_notify_after_import_and_do_not_replay_after_restart() {
        use super::super::{
            history,
            manual_tests::{publish, report},
        };
        use crate::{activity::notifications::engine::NotificationEngine, state::StateStore};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let engine =
            NotificationEngine::persistent(StateStore::default(), root.join("notifications.db"))
                .await
                .unwrap();
        let service = NotificationService::native(engine.clone());
        let mut publisher = Publisher {
            service: service.clone(),
            path: root.join("delivery.json"),
            first_scan: true,
        };
        // Upgrade from the worker-only coordinator: its initialized bit must not
        // turn an old manual ledger into a new notification backlog.
        std::fs::write(
            root.join("delivery.json"),
            r#"{"initialized":true,"seen":[],"incidents":{}}"#,
        )
        .unwrap();
        let mut reports = vec![report(root, 0, 1, false), report(root, 1, 0, true)];
        publish(root, &reports);
        publisher
            .reconcile(&history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
        assert!(
            engine.active().await.is_empty(),
            "old successful rebuilds import silently"
        );
        reports.push(report(root, 2, 1, false));
        publish(root, &reports);
        publisher
            .reconcile(&history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
        assert_eq!(engine.active().await.len(), 1);
        assert_eq!(engine.active().await[0].summary, "Manual rebuild failed");
        let mut publisher = Publisher {
            service,
            path: root.join("delivery.json"),
            first_scan: true,
        };
        publisher
            .reconcile(&history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
        assert_eq!(engine.active().await.len(), 1);
        reports.push(report(root, 3, 0, true));
        publish(root, &reports);
        publisher
            .reconcile(&history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
        assert_eq!(engine.active().await.len(), 2);
        assert!(
            engine
                .active()
                .await
                .iter()
                .any(|n| n.summary == "System rebuilt successfully")
        );
        publisher
            .reconcile(&history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
        assert_eq!(engine.active().await.len(), 2);
        reports.push(report(root, 4, 0, false));
        publish(root, &reports);
        publisher
            .reconcile(&history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
        assert_eq!(
            engine.active().await.len(),
            3,
            "explicit rebuild completion notifies even when unchanged"
        );
        assert!(
            engine
                .active()
                .await
                .iter()
                .any(|n| n.summary.contains("system unchanged"))
        );
    }

    fn entry(name: &str, incident: Option<&str>) -> Entry {
        let record: shelllist_update_status::RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"lane":"ai-tools","notification_owner":"desktop",
            "components":[{"name":name,"before":"1","after":"1","changed":false,"phase":if incident.is_some(){"failed"}else{"current"},"error":incident,"incident":incident}],
            "job":{"schema_version":1,"id":"run-1","operation":"ai-update","status":if incident.is_some(){"failed"}else{"completed"},"phase":"current",
            "started_at":1,"updated_at":2,"finished_at":2,"pid":1,"boot_id":"boot","process_start":"1","exit_code":0,"error":null}
        })).unwrap();
        Entry {
            failed_stage: None,
            deployed: None,
            issues: Vec::new(),
            manual_log: None,
            outcome: record.outcome().into(),
            title: record.title().into(),
            record,
        }
    }
    #[test]
    fn failures_are_per_incident_and_unrelated_success_does_not_clear_them() {
        let mut state = Delivery::default();
        let failed = entry("pi", Some("pi:build:a"));
        assert!(update_incidents(&mut state, &failed));
        assert!(!update_incidents(&mut state, &failed));
        assert!(!update_incidents(&mut state, &entry("codex", None)));
        assert_eq!(state.incidents.len(), 1);
        assert!(update_incidents(
            &mut state,
            &entry("codex", Some("codex:build:b"))
        ));
        assert!(!update_incidents(&mut state, &entry("pi", None)));
        assert_eq!(state.incidents.len(), 1);
        assert!(
            update_incidents(&mut state, &failed),
            "recurrence after recovery is new"
        );
    }
    #[test]
    fn deliberate_hold_and_control_c_are_not_failure_popups() {
        let mut held = entry("pi", Some("pi:hold:1"));
        held.record.components[0].phase = "held".into();
        assert!(!update_incidents(&mut Delivery::default(), &held));
        held.outcome = "interrupted".into();
        held.record.job.exit_code = Some(130);
        assert!(incident_pairs(&held).is_empty());
    }
}
