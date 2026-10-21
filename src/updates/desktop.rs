//! One session-owned publisher. Notification failures never affect update jobs.
use super::catalog::{Catalog, Entry};
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

#[derive(Default, Serialize, Deserialize)]
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
        let events = state.events(catalog);
        if let Some(last) = events.last() {
            self.send(last, events.len(), state.incidents.len()).await?;
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

    async fn send(&self, last: &Entry, count: usize, unresolved: usize) -> Result<()> {
        let catchup = self.first_scan || count > 1;
        let summary = if catchup {
            "Recent updates to review"
        } else {
            &last.title
        };
        let body = if catchup {
            format!(
                "{} update outcome(s); {} unresolved issue(s). Open Updates to review.",
                count, unresolved
            )
        } else {
            outcome_body(last)
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
        Ok(())
    }
}

impl Delivery {
    fn events<'a>(&mut self, catalog: &'a Catalog) -> Vec<&'a Entry> {
        let (worker_initial, manual_initial) = (!self.initialized, !self.manual_initialized);
        let initial = |entry: &Entry| {
            if entry.is_manual() {
                manual_initial
            } else {
                worker_initial
            }
        };
        let mut events = Vec::new();
        // Process chronologically; a later recovery can retire an earlier issue.
        for entry in catalog.entries.iter().rev().filter(|e| {
            e.outcome != "running" && e.record.notification_owner == NotificationOwner::Desktop
        }) {
            if self.seen.contains(&entry.record.job.id) {
                continue;
            }
            let new_problem = update_incidents(self, entry);
            let completed = matches!(entry.outcome.as_str(), "activated" | "staged" | "partial")
                || (entry.is_manual() && matches!(entry.outcome.as_str(), "current" | "completed"));
            if new_problem || (!initial(entry) && completed) {
                events.push(entry);
            }
            self.seen.insert(entry.record.job.id.clone());
        }
        self.initialized = true;
        self.manual_initialized = true;
        // Retention follows the producer's bounded catalog, not an unbounded outbox.
        self.seen
            .retain(|id| catalog.entries.iter().any(|e| &e.record.job.id == id));
        events.retain(|entry| {
            !initial(entry)
                || incident_pairs(entry)
                    .iter()
                    .any(|(key, value)| self.incidents.get(key) == Some(value))
        });
        events
    }
}

fn outcome_body(entry: &Entry) -> String {
    // No raw journal/error body in desktop popups.
    match entry.outcome.as_str() {
        _ if entry.is_problem() => "Review the recorded outcome and diagnostics in Updates.".into(),
        "staged" => {
            "The candidate is staged for the next boot, not activated in this session.".into()
        }
        "current" if entry.is_manual() => {
            "The rebuild completed without changing the recorded system.".into()
        }
        "completed" if entry.is_manual() => {
            "Rebuild command completed. Review the recorded deployment status in Updates.".into()
        }
        _ if entry.record.lane == "system" => {
            "System activation completed. Review the recorded run for details.".into()
        }
        _ => format!(
            "{} component(s) changed. Review the run for details.",
            entry.record.components.iter().filter(|c| c.changed).count()
        ),
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
        && entry.is_problem()
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
    use super::{
        Delivery, Entry, NotificationService, Publisher, incident_pairs, update_incidents,
    };
    async fn reconcile_manual(publisher: &mut Publisher, root: &std::path::Path) {
        publisher
            .reconcile(&super::super::history::load_catalog(
                &root.join("absent-worker"),
                Some(root),
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delivery_failure_does_not_hide_invalidation_and_unchanged_catalogs_retry() {
        use super::super::HistoryMonitor;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("delivery.json");
        std::fs::write(&path, "broken").unwrap();
        let mut monitor = HistoryMonitor {
            last_revision: None,
            publisher: Some(Publisher {
                service: NotificationService::swaync(),
                path: path.clone(),
                first_scan: true,
            }),
        };
        let store = crate::state::StateStore::default();
        let mut events = store.subscribe();
        for changed in [true, false] {
            assert!(
                monitor
                    .refresh(
                        &store,
                        root.path().join("absent-worker"),
                        Some(root.path().into())
                    )
                    .await
                    .is_err()
            );
            assert_eq!(events.try_recv().is_ok(), changed);
        }
        std::fs::remove_file(&path).unwrap();
        monitor
            .refresh(
                &store,
                root.path().join("absent-worker"),
                Some(root.path().into()),
            )
            .await
            .unwrap();
        assert!(
            path.exists(),
            "unchanged catalog still retries cursor persistence"
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn manual_rebuilds_notify_after_import_and_do_not_replay_after_restart() {
        use super::super::manual_tests::{publish, report};
        use crate::activity::notifications::engine::NotificationEngine;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let engine =
            NotificationEngine::persistent(Default::default(), root.join("notifications.db"))
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
        reconcile_manual(&mut publisher, root).await;
        assert!(
            engine.active().await.is_empty(),
            "old successful rebuilds import silently"
        );
        reports.push(report(root, 2, 1, false));
        publish(root, &reports);
        reconcile_manual(&mut publisher, root).await;
        assert_eq!(engine.active().await.len(), 1);
        assert_eq!(engine.active().await[0].summary, "Manual rebuild failed");
        let mut publisher = Publisher {
            service,
            path: root.join("delivery.json"),
            first_scan: true,
        };
        reconcile_manual(&mut publisher, root).await;
        assert_eq!(engine.active().await.len(), 1);
        reports.push(report(root, 3, 0, true));
        publish(root, &reports);
        reconcile_manual(&mut publisher, root).await;
        assert_eq!(engine.active().await.len(), 2);
        assert!(
            engine
                .active()
                .await
                .iter()
                .any(|n| n.summary == "System rebuilt successfully")
        );
        reconcile_manual(&mut publisher, root).await;
        assert_eq!(engine.active().await.len(), 2);
        reports.push(report(root, 4, 0, false));
        publish(root, &reports);
        reconcile_manual(&mut publisher, root).await;
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
        super::super::catalog::test_entry("run-1", 1, name, incident)
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
