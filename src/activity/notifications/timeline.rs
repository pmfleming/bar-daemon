//! Date buckets and bounded repeat stacks are projected from the complete native
//! catalog, never inferred from the frontend's current transport window.
use std::collections::BTreeMap;

use chrono::{Local, TimeZone};
use serde::Serialize;

use super::{
    center::{CenterQuery, Preview},
    history::Position,
};

pub(crate) const WINDOW: usize = 20;
const STACK_LIMIT: usize = 50;

#[derive(Debug, Serialize)]
pub(crate) struct DateBucket {
    pub key: String,
    pub count: usize,
}
#[derive(Debug, Serialize)]
pub(crate) struct TimelineEntry {
    pub key: String,
    pub preview: Preview,
    pub members: Vec<Position>,
}
#[derive(Debug, Serialize)]
pub(crate) struct Timeline {
    pub epoch: String,
    pub revision: String,
    pub query: String,
    pub app_key: String,
    pub dates: Vec<DateBucket>,
    pub date: String,
    pub count: usize,
    pub total_rows: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub anchor_reached: bool,
    pub entries: Vec<TimelineEntry>,
}
fn date(created: u64) -> String {
    Local
        .timestamp_millis_opt(created.min(i64::MAX as u64) as i64)
        .single()
        .map(|time| time.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}
pub(crate) fn project(
    rows: Vec<Preview>,
    query: &CenterQuery,
    epoch: &str,
    revision: u64,
) -> Timeline {
    let mut buckets = BTreeMap::<String, Vec<Preview>>::new();
    for row in rows {
        buckets
            .entry(date(row.created_unix_ms))
            .or_default()
            .push(row);
    }
    let dates = buckets
        .iter()
        .rev()
        .map(|(key, rows)| DateBucket {
            key: key.clone(),
            count: rows.len(),
        })
        .collect::<Vec<_>>();
    let selected = query
        .date
        .clone()
        .filter(|d| buckets.contains_key(d))
        .or_else(|| dates.first().map(|d| d.key.clone()))
        .unwrap_or_default();
    let rows = buckets.remove(&selected).unwrap_or_default();
    let count = rows.len();
    let mut entries = Vec::<TimelineEntry>::new();
    let mut stacks = BTreeMap::<String, usize>::new();
    for row in rows {
        let position = Position {
            id: row.id,
            created: row.created_unix_ms,
        };
        // Only exact, fully represented, action-free messages can stack. Never
        // infer conversation identity or equality from truncated preview text.
        if query.group_similar
            && let Some(key) = row.repeat_key.as_ref()
        {
            if let Some(index) = stacks.get(key).copied()
                && entries[index].members.len() < STACK_LIMIT
            {
                entries[index].members.push(position);
                continue;
            }
            stacks.insert(key.clone(), entries.len());
        }
        entries.push(TimelineEntry {
            key: format!("{}:{}", row.id, row.created_unix_ms),
            preview: row,
            members: vec![position],
        });
    }
    let total_rows = entries.len();
    let end = (query.offset + WINDOW).min(total_rows);
    let anchor_reached = query.timeline_anchor.as_ref().is_none_or(|key| {
        entries
            .iter()
            .position(|entry| &entry.key == key)
            .is_none_or(|index| index < end)
    });
    Timeline {
        epoch: epoch.into(),
        revision: revision.to_string(),
        query: query.query.clone(),
        app_key: query.app_key.clone().unwrap_or_default(),
        dates,
        date: selected,
        count,
        total_rows,
        offset: query.offset,
        next_offset: (end < total_rows).then_some(end),
        anchor_reached,
        entries: entries
            .into_iter()
            .skip(query.offset)
            .take(WINDOW)
            .collect(),
    }
}

pub(crate) fn repeat_key(
    summary: &str,
    body: &str,
    category: &str,
    urgency: u8,
    action_free: bool,
) -> Option<String> {
    (action_free
        && summary.chars().count() < 160
        && body.chars().count() < 240
        && category.chars().count() <= 256)
        .then(|| {
            serde_json::to_string(&(summary, body, category, urgency)).expect("strings serialize")
        })
}
