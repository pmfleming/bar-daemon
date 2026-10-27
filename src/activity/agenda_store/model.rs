//! Persisted facts only: inferred dates and system timezone never enter this model.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ItemKind {
    Event,
    Task,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScheduleMode {
    #[default]
    Date,
    Timed,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Schedule {
    pub mode: ScheduleMode,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub start_unix_ms: Option<i64>,
    pub end_unix_ms: Option<i64>,
    pub timezone: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Capabilities {
    pub write: bool,
    pub delete: bool,
    pub complete: bool,
}
impl Capabilities {
    pub fn local() -> Self {
        Self {
            write: true,
            delete: true,
            complete: true,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Link {
    pub id: String,
    pub label: String,
    pub url: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChecklistStep {
    pub id: String,
    pub text: String,
    pub checked: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventDetails {
    pub location: String,
    pub people: Vec<String>,
    pub status: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TaskDetails {
    pub priority: i64,
    pub status: String,
    pub completed: bool,
    pub completed_unix_ms: Option<i64>,
    pub checklist: Vec<ChecklistStep>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub id: String,
    pub kind: ItemKind,
    pub source_id: String,
    pub source_uid: Option<String>,
    pub revision: u64,
    pub title: String,
    pub notes: String,
    pub links: Vec<Link>,
    pub schedule: Schedule,
    pub created_unix_ms: i64,
    pub modified_unix_ms: i64,
    pub capabilities: Capabilities,
    pub event: Option<EventDetails>,
    pub task: Option<TaskDetails>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewItem {
    pub kind: ItemKind,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default)]
    pub event: Option<EventDetails>,
    #[serde(default)]
    pub task: Option<TaskDetails>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ItemPatch {
    pub title: Option<String>,
    pub notes: Option<String>,
    pub links: Option<Vec<Link>>,
    pub schedule: Option<Schedule>,
    pub event: Option<EventDetails>,
    pub task: Option<TaskDetails>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MutationResult {
    pub item: Option<Record>,
    pub catalog_revision: u64,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub items: Vec<Record>,
    pub catalog_revision: u64,
}

pub(super) fn bounded(value: &str, max: usize, name: &str) -> Result<()> {
    ensure!(
        value.len() <= max && !value.contains('\0'),
        "invalid {name}: exceeds bound or contains NUL"
    );
    Ok(())
}
pub(super) fn identifier(value: &str) -> Result<()> {
    bounded(value, 512, "identifier")?;
    ensure!(!value.trim().is_empty(), "empty identifier");
    Ok(())
}
impl Record {
    pub(crate) fn validate(&self) -> Result<()> {
        identifier(&self.id)?;
        identifier(&self.source_id)?;
        ensure!(
            self.revision > 0 && self.revision <= i64::MAX as u64,
            "invalid revision"
        );
        bounded(&self.title, 4096, "title")?;
        ensure!(!self.title.trim().is_empty(), "title required");
        bounded(&self.notes, 262144, "notes")?;
        if let Some(uid) = &self.source_uid {
            identifier(uid)?;
        }
        self.schedule
            .validate(self.kind == ItemKind::Event && !self.capabilities.write)?;
        ensure!(self.links.len() <= 128, "too many links");
        let mut ids = HashSet::new();
        for link in &self.links {
            identifier(&link.id)?;
            ensure!(ids.insert(&link.id), "duplicate link id");
            bounded(&link.label, 4096, "link label")?;
            bounded(&link.url, 8192, "link URL")?;
            ensure!(
                link.url.starts_with("https://")
                    || link.url.starts_with("http://")
                    || link.url.starts_with("mailto:"),
                "unsupported link URL scheme"
            );
        }
        match self.kind {
            ItemKind::Event => {
                ensure!(
                    !self.capabilities.complete,
                    "event cannot have completion capability"
                );
                ensure!(self.task.is_none(), "event cannot contain task details");
                let event = self
                    .event
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("event details required"))?;
                bounded(&event.location, 8192, "location")?;
                bounded(&event.status, 128, "status")?;
                ensure!(event.people.len() <= 256, "too many people");
                for person in &event.people {
                    bounded(person, 4096, "person")?;
                }
            }
            ItemKind::Task => {
                ensure!(self.event.is_none(), "task cannot contain event details");
                let task = self
                    .task
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("task details required"))?;
                bounded(&task.status, 128, "status")?;
                // Historical completed=true/null timestamps are preserved, not invented.
                ensure!(
                    task.completed || task.completed_unix_ms.is_none(),
                    "open task has completion timestamp"
                );
                ensure!(task.checklist.len() <= 512, "too many checklist steps");
                let mut ids = HashSet::new();
                for step in &task.checklist {
                    identifier(&step.id)?;
                    ensure!(ids.insert(&step.id), "duplicate checklist id");
                    bounded(&step.text, 8192, "checklist text")?;
                }
            }
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= 1048576,
            "record payload too large"
        );
        Ok(())
    }
}
fn collection_bounds(
    links: Option<&Vec<Link>>,
    event: Option<&EventDetails>,
    task: Option<&TaskDetails>,
) -> Result<()> {
    if let Some(links) = links {
        ensure!(links.len() <= 128, "too many links");
        for link in links {
            identifier(&link.id)?;
            bounded(&link.label, 4096, "link label")?;
            bounded(&link.url, 8192, "link URL")?;
        }
    }
    if let Some(event) = event {
        bounded(&event.location, 8192, "location")?;
        bounded(&event.status, 128, "status")?;
        ensure!(event.people.len() <= 256, "too many people");
        for person in &event.people {
            bounded(person, 4096, "person")?;
        }
    }
    if let Some(task) = task {
        bounded(&task.status, 128, "status")?;
        ensure!(task.checklist.len() <= 512, "too many checklist steps");
        for step in &task.checklist {
            identifier(&step.id)?;
            bounded(&step.text, 8192, "checklist text")?;
        }
    }
    Ok(())
}
impl NewItem {
    pub(super) fn validate_bounds(&self) -> Result<()> {
        bounded(&self.title, 4096, "title")?;
        bounded(&self.notes, 262144, "notes")?;
        self.schedule.validate(false)?;
        collection_bounds(Some(&self.links), self.event.as_ref(), self.task.as_ref())
    }
}
impl ItemPatch {
    pub(super) fn validate_bounds(&self) -> Result<()> {
        if let Some(title) = &self.title {
            bounded(title, 4096, "title")?;
        }
        if let Some(notes) = &self.notes {
            bounded(notes, 262144, "notes")?;
        }
        if let Some(schedule) = &self.schedule {
            schedule.validate(false)?;
        }
        collection_bounds(self.links.as_ref(), self.event.as_ref(), self.task.as_ref())
    }
}
impl Schedule {
    pub(crate) fn validate(&self, allow_instant: bool) -> Result<()> {
        match self.mode {
            ScheduleMode::Date => {
                ensure!(
                    self.start_unix_ms.is_none()
                        && self.end_unix_ms.is_none()
                        && self.timezone.is_none(),
                    "date schedule contains timed values"
                );
                for value in [&self.start_date, &self.end_date].into_iter().flatten() {
                    ensure!(
                        value.len() == 10
                            && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
                        "invalid civil date"
                    );
                }
                if let (Some(start), Some(end)) = (&self.start_date, &self.end_date) {
                    ensure!(start <= end, "reversed date range");
                }
            }
            ScheduleMode::Timed => {
                if let Some(zone) = &self.timezone {
                    bounded(zone, 128, "timezone")?;
                }
                ensure!(
                    self.start_date.is_none() && self.end_date.is_none(),
                    "timed schedule contains civil dates"
                );
                self.timezone
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("timed schedule needs IANA zone"))?
                    .parse::<chrono_tz::Tz>()?;
                for value in [self.start_unix_ms, self.end_unix_ms].into_iter().flatten() {
                    ensure!(
                        chrono::DateTime::from_timestamp_millis(value).is_some(),
                        "invalid instant"
                    );
                }
                if let (Some(start), Some(end)) = (self.start_unix_ms, self.end_unix_ms) {
                    ensure!(
                        start < end || (allow_instant && start == end),
                        "reversed or empty timed range"
                    );
                }
            }
        }
        Ok(())
    }
}
