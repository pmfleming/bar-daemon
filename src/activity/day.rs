//! Legacy range framing; membership delegates to the same native agenda projection.
use super::{
    agenda_adapter,
    agenda_projection::Projection,
    agenda_store::model::{Schedule, ScheduleMode},
    model::{ActivityDay, ActivityEvent, TodoItem},
};
use anyhow::{Context, Result, ensure};
use chrono::{NaiveDate, TimeZone};
use std::collections::BTreeMap;

pub(super) struct QueryRange {
    pub first: NaiveDate,
    pub last: NaiveDate,
}
impl QueryRange {
    pub fn new<T: TimeZone>(from: i64, to: i64, zone: &T) -> Result<Self> {
        ensure!(to > from, "to_unix_ms must be greater than from_unix_ms");
        let duration = to
            .checked_sub(from)
            .context("activity range duration overflows")?;
        ensure!(
            duration <= 370 * 24 * 60 * 60 * 1000,
            "activity range cannot exceed 370 days"
        );
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
        Ok(Self { first, last })
    }
}

/// Offline legacy protocol fixtures use production projection, not a second
/// due-day-only/undated-open policy that could disagree with agenda markers.
pub(super) fn project<T: TimeZone>(
    events: &[ActivityEvent],
    todos: &[TodoItem],
    first: NaiveDate,
    last: NaiveDate,
    today: NaiveDate,
    zone: &T,
) -> BTreeMap<String, ActivityDay> {
    let p = Projection {
        today,
        zone: zone.clone(),
    };
    let event_schedules: Vec<_> = events
        .iter()
        .map(|e| {
            agenda_adapter::imported(e)
                .expect("validated example event")
                .schedule
        })
        .collect();
    let todo_schedules: Vec<_> = todos
        .iter()
        .map(|t| {
            if let Some(date) = &t.due_date {
                Schedule {
                    end_date: Some(date.clone()),
                    ..Schedule::default()
                }
            } else {
                Schedule {
                    mode: ScheduleMode::Timed,
                    end_unix_ms: t.due_unix_ms,
                    timezone: Some("UTC".into()),
                    ..Schedule::default()
                }
            }
        })
        .collect();
    std::iter::successors(Some(first), |d| d.succ_opt())
        .take_while(|d| *d <= last)
        .map(|day| {
            let event_ids = events
                .iter()
                .zip(&event_schedules)
                .filter(|(_, s)| p.includes(s, day))
                .map(|(e, _)| e.id.clone())
                .collect();
            let todo_ids = todos
                .iter()
                .zip(&todo_schedules)
                .filter(|(_, s)| p.includes(s, day))
                .map(|(t, _)| t.id.clone())
                .collect();
            (
                day.to_string(),
                ActivityDay {
                    event_ids,
                    todo_ids,
                },
            )
        })
        .collect()
}
