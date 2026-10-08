//! Calendar membership uses local dates, never a fixed 24-hour duration.
use super::model::{ActivityDay, ActivityEvent, TodoItem};
use chrono::{NaiveDate, TimeZone};
use std::collections::BTreeMap;

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
    let mut date = first;
    while date <= last {
        let key = date.to_string();
        let event_ids = events
            .iter()
            .filter(|event| {
                if event.all_day {
                    if let Some(start) = event.start_date.as_deref() {
                        return start <= key.as_str()
                            && event.end_date.as_deref().unwrap_or(start) > key.as_str();
                    }
                }
                let last_instant = if event.end_unix_ms == event.start_unix_ms {
                    event.start_unix_ms // A valid zero-duration VEVENT is an instant.
                } else {
                    event.end_unix_ms.saturating_sub(1)
                };
                event.end_unix_ms >= event.start_unix_ms
                    && date_at(event.start_unix_ms).is_some_and(|d| d <= date)
                    && date_at(last_instant).is_some_and(|d| d >= date)
            })
            .map(|e| e.id.clone())
            .collect();
        let todo_ids = todos
            .iter()
            .filter(|todo| {
                if let Some(due) = &todo.due_date {
                    return due == &key;
                }
                if let Some(due) = todo.due_unix_ms {
                    return date_at(due) == Some(date);
                }
                date == today && !todo.completed
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
        let Some(next) = date.succ_opt() else {
            break;
        };
        date = next;
    }
    days
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    #[test]
    fn membership_handles_dst_exclusive_ends_and_undated_todos() {
        let zone = chrono_tz::Europe::Berlin;
        let ms = |v: &str| DateTime::parse_from_rfc3339(v).unwrap().timestamp_millis();
        for (day, next, late, included) in [
            (
                "2026-03-29",
                "2026-03-30",
                "2026-03-30T00:30:00+02:00",
                false,
            ),
            (
                "2026-10-25",
                "2026-10-26",
                "2026-10-25T23:30:00+01:00",
                true,
            ),
        ] {
            let date = day.parse().unwrap();
            let events = vec![
                ActivityEvent {
                    id: "timed".into(),
                    start_unix_ms: ms(late),
                    end_unix_ms: ms(late) + 60000,
                    ..Default::default()
                },
                ActivityEvent {
                    id: "all-day".into(),
                    all_day: true,
                    start_date: Some(day.into()),
                    end_date: Some(next.into()),
                    ..Default::default()
                },
            ];
            let todos = vec![
                TodoItem {
                    id: "undated".into(),
                    ..Default::default()
                },
                TodoItem {
                    id: "done".into(),
                    completed: true,
                    ..Default::default()
                },
            ];
            let days = project(&events, &todos, date, next.parse().unwrap(), date, &zone);
            assert_eq!(days[day].event_ids.contains(&"timed".into()), included);
            assert!(days[day].event_ids.contains(&"all-day".into()));
            assert!(!days[next].event_ids.contains(&"all-day".into()));
            assert_eq!(days[day].todo_ids, ["undated"]);
            assert!(days[next].todo_ids.is_empty());
        }
    }
}
