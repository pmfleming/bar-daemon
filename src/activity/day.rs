//! Calendar membership uses local dates, never a fixed 24-hour duration.
use super::model::{ActivityDay, ActivityEvent, TodoItem};
use anyhow::{Context, Result, ensure};
use chrono::{NaiveDate, TimeZone};
use std::{collections::BTreeMap, ops::Range};

/// A validated half-open instant range and the local dates it touches.
pub(super) struct QueryRange {
    instants: Range<i64>,
    pub first: NaiveDate,
    pub last: NaiveDate,
    date_keys: (String, String),
}

impl QueryRange {
    pub fn new<T: TimeZone>(from: i64, to: i64, zone: &T) -> Result<Self> {
        ensure!(to > from, "to_unix_ms must be greater than from_unix_ms");
        let duration = to
            .checked_sub(from)
            .context("activity range duration overflows")?;
        ensure!(
            duration <= 370 * 24 * 60 * 60 * 1_000,
            "activity range cannot exceed 370 days"
        );
        // Include partial days and DST using local dates, not fixed 24-hour steps.
        let first = zone
            .timestamp_millis_opt(from)
            .single()
            .context("activity range start is outside supported dates")?
            .date_naive();
        let last = zone
            .timestamp_millis_opt(to - 1)
            .single()
            .context("activity range end is outside supported dates")?
            .date_naive();
        Ok(Self {
            instants: from..to,
            first,
            last,
            date_keys: (first.to_string(), last.to_string()),
        })
    }

    pub fn includes_event(&self, event: &ActivityEvent) -> bool {
        event.start_unix_ms < self.instants.end
            && if event.end_unix_ms == event.start_unix_ms {
                self.instants.contains(&event.start_unix_ms)
            } else {
                event.end_unix_ms > self.instants.start
            }
    }

    pub fn includes_todo(&self, todo: &TodoItem) -> bool {
        match (todo.due_unix_ms, todo.due_date.as_deref()) {
            (Some(due), _) => self.instants.contains(&due),
            (None, Some(date)) => {
                date >= self.date_keys.0.as_str() && date <= self.date_keys.1.as_str()
            }
            (None, None) => true,
        }
    }
}

pub(super) fn project<T: TimeZone>(
    events: &[ActivityEvent],
    todos: &[TodoItem],
    first: NaiveDate,
    last: NaiveDate,
    today: NaiveDate,
    zone: &T,
) -> BTreeMap<String, ActivityDay> {
    let date_at = |ms| {
        zone.timestamp_millis_opt(ms)
            .single()
            .map(|v| v.date_naive())
    };
    let mut days = BTreeMap::new();
    for date in
        std::iter::successors(Some(first), |date| date.succ_opt()).take_while(|date| *date <= last)
    {
        let key = date.to_string();
        let event_ids = events
            .iter()
            .filter(|event| event_on_date(event, date, &key, date_at))
            .map(|e| e.id.clone())
            .collect();
        let todo_ids = todos
            .iter()
            .filter(|todo| match (&todo.due_date, todo.due_unix_ms) {
                (Some(due), _) => due == &key,
                (_, Some(due)) => date_at(due) == Some(date),
                _ => date == today && !todo.completed,
            })
            .map(|t| t.id.clone())
            .collect();
        days.insert(
            key,
            ActivityDay {
                event_ids,
                todo_ids,
            },
        );
    }
    days
}

fn event_on_date(
    event: &ActivityEvent,
    date: NaiveDate,
    key: &str,
    date_at: impl Fn(i64) -> Option<NaiveDate>,
) -> bool {
    if let (true, Some(start)) = (event.all_day, event.start_date.as_deref()) {
        return start <= key && event.end_date.as_deref().unwrap_or(start) > key;
    }
    // A zero-duration VEVENT is an instant; other ends are exclusive.
    let last_instant = event.end_unix_ms.saturating_sub(1).max(event.start_unix_ms);
    event.end_unix_ms >= event.start_unix_ms
        && date_at(event.start_unix_ms).is_some_and(|d| d <= date)
        && date_at(last_instant).is_some_and(|d| d >= date)
}
