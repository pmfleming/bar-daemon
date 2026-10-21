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
        let mut events = Vec::new();
        // Process chronologically; a later recovery can retire an earlier issue.
        for entry in catalog.entries.iter().rev().filter(|e| {
            e.outcome != "running" && e.record.notification_owner == NotificationOwner::Desktop
        }) {
            if state.seen.contains(&entry.record.job.id) {
                continue;
            }
            let new_problem = update_incidents(&mut state, entry);
            if new_problem
                || (!initial
                    && matches!(entry.outcome.as_str(), "activated" | "staged" | "partial"))
            {
                events.push(entry);
            }
            state.seen.insert(entry.record.job.id.clone());
        }
        state.initialized = true;
        // Retention follows the producer's bounded catalog, not an unbounded outbox.
        state
            .seen
            .retain(|id| catalog.entries.iter().any(|e| &e.record.job.id == id));
        if initial {
            events.retain(|e| {
                incident_pairs(e)
                    .iter()
                    .any(|(key, value)| state.incidents.get(key) == Some(value))
            });
        }
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
        && matches!(entry.outcome.as_str(), "failed" | "blocked" | "interrupted")
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
    fn entry(name: &str, incident: Option<&str>) -> Entry {
        let record: shelllist_update_status::RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"lane":"ai-tools","notification_owner":"desktop",
            "components":[{"name":name,"before":"1","after":"1","changed":false,"phase":if incident.is_some(){"failed"}else{"current"},"error":incident,"incident":incident}],
            "job":{"schema_version":1,"id":"run-1","operation":"ai-update","status":if incident.is_some(){"failed"}else{"completed"},"phase":"current",
            "started_at":1,"updated_at":2,"finished_at":2,"pid":1,"boot_id":"boot","process_start":"1","exit_code":0,"error":null}
        })).unwrap();
        Entry {
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
