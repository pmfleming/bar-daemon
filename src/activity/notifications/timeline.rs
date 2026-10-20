//! Date buckets and bounded repeat stacks are projected from the complete native
//! catalog, never inferred from the frontend's current transport window.
use std::collections::BTreeMap;

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use serde::Serialize;

use super::{
    center::{CenterQuery, Preview, RepeatKey},
    history::{HistoryError, Position},
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub period_day: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent: Option<Vec<Preview>>,
    pub date: String,
    pub count: usize,
    pub total_rows: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub anchor_reached: bool,
    pub entries: Vec<TimelineEntry>,
}
fn local_date(created: u64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(created.min(i64::MAX as u64) as i64)
        .single()
        .map(|time| time.date_naive())
}
fn date(created: u64) -> String {
    local_date(created)
        .map(|d| d.to_string())
        .unwrap_or_default()
}
const PERIODS: [&str; 4] = ["today", "week", "month", "older"];
fn period(created: u64, today: NaiveDate) -> &'static str {
    let Some(day) = local_date(created) else {
        return "older";
    };
    let week = today
        .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
        .unwrap_or(today);
    if day >= today {
        "today"
    } else if day >= week {
        "week"
    } else if (day.year(), day.month()) == (today.year(), today.month()) {
        "month"
    } else {
        "older"
    }
}
pub(crate) fn project_at(
    rows: Vec<Preview>,
    query: &CenterQuery,
    epoch: &str,
    revision: u64,
    today: NaiveDate,
) -> Result<Timeline, HistoryError> {
    let period_day = if query.period_groups {
        today.to_string()
    } else {
        String::new()
    };
    // Bucket membership changes at local midnight even without a notification
    // revision. Never append a window from a different calendar snapshot.
    if query.period_groups && query.offset > 0 && query.period_day.as_deref() != Some(&period_day) {
        return Err(HistoryError::Stale);
    }
    let mut rows = rows.into_iter();
    let recent = query.period_groups.then(|| rows.by_ref().take(3).collect());
    let mut buckets = BTreeMap::<String, Vec<Preview>>::new();
    for row in rows {
        let key = if query.period_groups {
            period(row.created_unix_ms, today).into()
        } else {
            date(row.created_unix_ms)
        };
        buckets.entry(key).or_default().push(row);
    }
    let dates = if query.period_groups {
        PERIODS
            .iter()
            .map(|key| DateBucket {
                key: (*key).into(),
                count: buckets.get(*key).map_or(0, Vec::len),
            })
            .collect()
    } else {
        buckets
            .iter()
            .rev()
            .map(|(key, rows)| DateBucket {
                key: key.clone(),
                count: rows.len(),
            })
            .collect::<Vec<_>>()
    };
    let selected = if query.period_groups {
        query.date.clone().unwrap_or_default()
    } else {
        query
            .date
            .clone()
            .filter(|d| buckets.contains_key(d))
            .or_else(|| dates.first().map(|d| d.key.clone()))
            .unwrap_or_default()
    };
    let rows = buckets.remove(&selected).unwrap_or_default();
    let count = rows.len();
    let entries = stack(rows, query.group_similar);
    let total_rows = entries.len();
    let end = (query.offset + WINDOW).min(total_rows);
    let anchor_reached = query.timeline_anchor.as_ref().is_none_or(|key| {
        entries
            .iter()
            .position(|entry| &entry.key == key)
            .is_none_or(|index| index < end)
    });
    Ok(Timeline {
        epoch: epoch.into(),
        revision: revision.to_string(),
        query: query.query.clone(),
        app_key: query.app_key.clone().unwrap_or_default(),
        dates,
        period_day,
        recent,
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
    })
}

fn stack(rows: Vec<Preview>, group_similar: bool) -> Vec<TimelineEntry> {
    let mut entries = Vec::<TimelineEntry>::new();
    let mut stacks = BTreeMap::<(String, RepeatKey), usize>::new();
    for mut row in rows {
        let position = Position {
            id: row.id,
            created: row.created_unix_ms,
        };
        // Only exact, fully represented, action-free messages can stack. Never
        // infer conversation identity or equality from truncated preview text.
        if group_similar && let Some(key) = row.repeat_key.take() {
            let key = (date(row.created_unix_ms), key);
            if let Some(index) = stacks.get(&key).copied()
                && entries[index].members.len() < STACK_LIMIT
            {
                entries[index].members.push(position);
                continue;
            }
            stacks.insert(key, entries.len());
        }
        entries.push(TimelineEntry {
            key: format!("{}:{}", row.id, row.created_unix_ms),
            preview: row,
            members: vec![position],
        });
    }
    entries
}
