//! Deterministic observations for offline protocol examples, projected by the live day policy.
use super::{
    day,
    model::{ActivityEvent, ActivityRange, TodoItem},
};
use chrono::{NaiveDate, TimeZone, Utc};

pub(crate) fn empty_range() -> ActivityRange {
    let first = NaiveDate::from_ymd_opt(2026, 1, 17).unwrap();
    let last = NaiveDate::from_ymd_opt(2026, 1, 19).unwrap();
    ActivityRange {
        from_unix_ms: Utc
            .with_ymd_and_hms(2026, 1, 17, 0, 0, 0)
            .unwrap()
            .timestamp_millis(),
        to_unix_ms: Utc
            .with_ymd_and_hms(2026, 1, 20, 0, 0, 0)
            .unwrap()
            .timestamp_millis(),
        days: day::project(
            &[],
            &[],
            first,
            last,
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            &Utc,
        ),
        local_date: "2026-01-15".into(),
        ..Default::default()
    }
}

pub(crate) fn range() -> ActivityRange {
    let today = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
    let from_unix_ms = Utc
        .with_ymd_and_hms(2026, 1, 14, 0, 0, 0)
        .unwrap()
        .timestamp_millis();
    let to_unix_ms = Utc
        .with_ymd_and_hms(2026, 1, 17, 0, 0, 0)
        .unwrap()
        .timestamp_millis();
    let events = vec![ActivityEvent {
        id: "calendar-example".into(),
        source_id: "local-calendar".into(),
        calendar_name: "Personal".into(),
        title: "Architecture review".into(),
        start_unix_ms: from_unix_ms + 36 * 3_600_000,
        end_unix_ms: from_unix_ms + 37 * 3_600_000,
        ..Default::default()
    }];
    let todos = vec![TodoItem {
        id: "local-example".into(),
        source_id: "local".into(),
        title: "Plan review".into(),
        due_date: Some(today.to_string()),
        created_unix_ms: from_unix_ms,
        ..Default::default()
    }];
    let days = day::project(
        &events,
        &todos,
        today.pred_opt().unwrap(),
        today.succ_opt().unwrap(),
        today,
        &Utc,
    );
    let busy_dates = days
        .iter()
        .filter(|(_, day)| !day.event_ids.is_empty() || !day.todo_ids.is_empty())
        .map(|(date, _)| date.clone())
        .collect();
    ActivityRange {
        from_unix_ms,
        to_unix_ms,
        events,
        todos,
        busy_dates,
        days,
        local_date: today.to_string(),
    }
}
