//! Native ordered catalog, whole-catalog search and fenced bounded windows.
use super::{
    agenda_projection::{Projection, date},
    agenda_store::model::{ItemKind, Record},
};
use anyhow::{Result, ensure};
use chrono::{NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ListRequest {
    pub query: String,
    pub day: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CatalogInfo {
    pub catalog_revision: u64,
    pub import_revision: u64,
    pub today: String,
    pub timezone: String,
}
#[derive(Serialize)]
pub(crate) struct Summary {
    pub summary: bool,
    pub id: String,
    pub kind: ItemKind,
    pub source_id: String,
    pub source_uid: Option<String>,
    pub revision: u64,
    pub title: String,
    pub schedule: super::agenda_store::model::Schedule,
    pub created_unix_ms: i64,
    pub modified_unix_ms: i64,
    pub capabilities: super::agenda_store::model::Capabilities,
    pub event: Option<EventSummary>,
    pub task: Option<TaskSummary>,
}
#[derive(Serialize)]
pub(crate) struct EventSummary {
    status: String,
    location: String,
}
#[derive(Serialize)]
pub(crate) struct TaskSummary {
    priority: i64,
    status: String,
    completed: bool,
    completed_unix_ms: Option<i64>,
}
impl From<&Record> for Summary {
    fn from(item: &Record) -> Self {
        Self {
            summary: true,
            id: item.id.clone(),
            kind: item.kind,
            source_id: item.source_id.clone(),
            source_uid: item.source_uid.clone(),
            revision: item.revision,
            title: item.title.clone(),
            schedule: item.schedule.clone(),
            created_unix_ms: item.created_unix_ms,
            modified_unix_ms: item.modified_unix_ms,
            capabilities: item.capabilities.clone(),
            event: item.event.as_ref().map(|e| EventSummary {
                status: e.status.clone(),
                location: e.location.clone(),
            }),
            task: item.task.as_ref().map(|t| TaskSummary {
                priority: t.priority,
                status: t.status.clone(),
                completed: t.completed,
                completed_unix_ms: t.completed_unix_ms,
            }),
        }
    }
}
#[derive(Serialize)]
pub(crate) struct ListResult {
    pub items: Vec<Summary>,
    pub next_cursor: Option<String>,
    pub catalog_revision: u64,
    pub today: String,
    pub timezone: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarkersRequest {
    pub from_date: String,
    pub to_date: String,
}
#[derive(Serialize)]
pub(crate) struct Marker {
    pub date: String,
    pub event: bool,
    pub task: bool,
}
#[derive(Serialize)]
pub(crate) struct MarkersResult {
    pub days: Vec<Marker>,
    pub catalog_revision: u64,
    pub today: String,
    pub timezone: String,
}
#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct Fence {
    query: String,
    day: Option<String>,
    today: String,
    timezone: String,
    revision: u64,
    imports: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    fence: Fence,
    offset: usize,
}

fn order<'a, T: TimeZone>(item: &'a Record, p: &Projection<T>) -> (u8, i64, &'a str) {
    use super::agenda_store::model::ScheduleMode;
    let (start, end) = p.stored_dates(&item.schedule);
    let (start_key, end_key) = match item.schedule.mode {
        ScheduleMode::Date => (
            start.and_then(|d| p.midnight(d)),
            // Civil ends are inclusive: compare their following day boundary
            // against timed exclusive ends, without discarding instant precision.
            end.and_then(|d| d.succ_opt()).and_then(|d| p.midnight(d)),
        ),
        ScheduleMode::Timed => (item.schedule.start_unix_ms, item.schedule.end_unix_ms),
    };
    if p.includes(&item.schedule, p.today) {
        return (
            0,
            start_key.or_else(|| p.midnight(p.today)).unwrap_or(0),
            &item.id,
        );
    }
    match start.or(end) {
        Some(d) if d > p.today => (1, start_key.or(end_key).unwrap_or(0), &item.id),
        Some(_) => (
            2,
            end_key.or(start_key).unwrap_or(0).saturating_neg(),
            &item.id,
        ),
        None => (3, 0, &item.id),
    }
}
fn search_text(item: &Record) -> String {
    let mut text = format!("{} {}", item.title, item.notes);
    if let Some(e) = &item.event {
        text.push_str(&format!(
            " {} {} {}",
            e.location,
            e.people.join(" "),
            e.status
        ));
    }
    if let Some(t) = &item.task {
        for s in &t.checklist {
            text.push(' ');
            text.push_str(&s.text);
        }
    }
    for link in &item.links {
        text.push_str(&format!(" {} {}", link.label, link.url));
    }
    text.to_lowercase()
}
pub(crate) struct Catalog {
    pub items: Vec<Record>,
    search: Vec<String>,
    membership: Vec<Option<super::agenda_projection::Membership>>,
    by_id: std::collections::HashMap<String, usize>,
}
impl Catalog {
    pub fn new<T: TimeZone>(mut items: Vec<Record>, p: &Projection<T>) -> Self {
        items.sort_by(|a, b| order(a, p).cmp(&order(b, p)));
        let search = items.iter().map(search_text).collect();
        let by_id = items
            .iter()
            .enumerate()
            .map(|(n, item)| (item.id.clone(), n))
            .collect();
        let membership = items
            .iter()
            .map(|item| p.membership(&item.schedule))
            .collect();
        Self {
            items,
            search,
            membership,
            by_id,
        }
    }
    pub fn get(&self, id: &str) -> Option<&Record> {
        self.by_id.get(id).map(|n| &self.items[*n])
    }
    pub fn on_day(
        &self,
        day: NaiveDate,
        bounds: Option<(i64, i64)>,
    ) -> impl Iterator<Item = &Record> {
        self.items
            .iter()
            .zip(&self.membership)
            .filter(move |(_, m)| m.is_some_and(|m| m.includes(day, bounds)))
            .map(|(item, _)| item)
    }
}
pub(crate) struct CachedCatalog {
    pub revision: u64,
    pub import_revision: u64,
    pub today: NaiveDate,
    pub timezone: String,
    pub imports: String,
    pub catalog: std::sync::Arc<Catalog>,
}
pub(crate) fn fingerprint(items: &[Record]) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(items).expect("record serialization"))
    )
}
pub(crate) fn list<T: TimeZone>(
    catalog: &Catalog,
    revision: u64,
    imports: &str,
    p: &Projection<T>,
    timezone: &str,
    request: ListRequest,
) -> Result<ListResult> {
    ensure!(request.query.len() <= 4096, "query too long");
    let limit = request.limit.unwrap_or(100);
    ensure!((1..=100).contains(&limit), "limit must be 1..100");
    let day = request.day.as_deref().map(date).transpose()?;
    let query = request.query.trim().to_lowercase();
    let fence = Fence {
        query: query.clone(),
        day: request.day,
        today: p.today.to_string(),
        timezone: timezone.into(),
        revision,
        imports: imports.into(),
    };
    let offset = if let Some(cursor) = request.cursor {
        ensure!(cursor.len() <= 65536, "cursor too long");
        let cursor: Cursor = serde_json::from_str(&cursor)?;
        ensure!(cursor.fence == fence, "stale agenda cursor; restart query");
        cursor.offset
    } else {
        0
    };
    // Accepted catalog/search text is cached by revision + today + zone.
    // Keystrokes scan cached text, never re-read or re-sort stored history.
    let bounds = day.and_then(|d| p.bounds(d));
    let candidates: Vec<_> = catalog
        .items
        .iter()
        .zip(&catalog.search)
        .zip(&catalog.membership)
        .filter(|((_, text), membership)| {
            day.is_none_or(|d| membership.is_some_and(|m| m.includes(d, bounds)))
                && query.split_whitespace().all(|token| text.contains(token))
        })
        .map(|((item, _), _)| item)
        .collect();
    ensure!(offset <= candidates.len(), "invalid cursor offset");
    let mut end = offset;
    let mut summaries = Vec::new();
    let mut bytes = 0;
    // Transport accepts at most 4 MiB. Reserve ample space for the envelope,
    // escaped query cursor and projection identity; list payload stays <2 MiB.
    for item in candidates.iter().skip(offset).take(limit) {
        let summary = Summary::from(*item);
        let length = serde_json::to_vec(&summary)?.len() + 1;
        if bytes + length > 2 * 1024 * 1024 - 128 * 1024 {
            break;
        }
        bytes += length;
        end += 1;
        summaries.push(summary);
    }
    ensure!(
        end > offset || offset == candidates.len(),
        "agenda summary exceeds transport budget"
    );
    let next_cursor = (end < candidates.len())
        .then(|| serde_json::to_string(&Cursor { fence, offset: end }))
        .transpose()?;
    Ok(ListResult {
        items: summaries,
        next_cursor,
        catalog_revision: revision,
        today: p.today.to_string(),
        timezone: timezone.into(),
    })
}
pub(crate) fn markers<T: TimeZone>(
    catalog: &Catalog,
    revision: u64,
    p: &Projection<T>,
    timezone: &str,
    request: MarkersRequest,
) -> Result<MarkersResult> {
    let first = date(&request.from_date)?;
    let last = date(&request.to_date)?;
    let length = (last - first).num_days() + 1;
    ensure!(
        (1..=370).contains(&length),
        "marker range must be 1..370 inclusive days"
    );
    let days = (0..length)
        .map(|offset| {
            let day: NaiveDate = first + chrono::Duration::days(offset);
            let mut marker = Marker {
                date: day.to_string(),
                event: false,
                task: false,
            };
            let bounds = p.bounds(day);
            for (item, membership) in catalog.items.iter().zip(&catalog.membership) {
                if membership.is_some_and(|m| m.includes(day, bounds)) {
                    match item.kind {
                        ItemKind::Event => marker.event = true,
                        ItemKind::Task => marker.task = true,
                    }
                    if marker.event && marker.task {
                        break;
                    }
                }
            }
            marker
        })
        .collect();
    Ok(MarkersResult {
        days,
        catalog_revision: revision,
        today: p.today.to_string(),
        timezone: timezone.into(),
    })
}
