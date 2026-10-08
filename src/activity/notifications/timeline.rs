//! Date buckets and bounded repeat stacks are projected from the complete native
//! catalog, never inferred from the frontend's current transport window.
use std::collections::BTreeMap;

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use serde::Serialize;

use super::{
    center::{CenterQuery, Preview},
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
    } else if day >= today.with_day(1).expect("first day exists") {
        "month"
    } else {
        "older"
    }
}
pub(crate) fn project(
    rows: Vec<Preview>,
    query: &CenterQuery,
    epoch: &str,
    revision: u64,
) -> Result<Timeline, HistoryError> {
    project_at(rows, query, epoch, revision, Local::now().date_naive())
}
fn project_at(
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
    let mut entries = Vec::<TimelineEntry>::new();
    let mut stacks = BTreeMap::<(String, String), usize>::new();
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
            let key = (date(row.created_unix_ms), key.clone());
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

#[cfg(test)]
mod tests {
    use super::super::center::PreviewHints;
    use super::*;

    fn day(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }
    fn preview(id: u32, date: &str) -> Preview {
        let created = Local
            .from_local_datetime(&day(date).and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis() as u64
            + u64::from(id);
        Preview {
            id,
            created_unix_ms: created,
            app_key: "named:Chat".into(),
            app_name: "Chat".into(),
            app_icon: String::new(),
            identity_icon: String::new(),
            hints: PreviewHints {
                desktop_entry: String::new(),
                image_path: String::new(),
            },
            summary: "Repeated".into(),
            body: "Body".into(),
            closed_unix_ms: None,
            snoozed_until_unix_ms: None,
            history_id: None,
            matches: true,
            group_key: String::new(),
            repeat_key: Some("exact".into()),
        }
    }
    fn period_query() -> CenterQuery {
        serde_json::from_value(
            serde_json::json!({"view":"timeline", "app_key":"named:Chat", "period_groups":true}),
        )
        .unwrap()
    }

    #[test]
    fn recent_three_are_individual_and_excluded_from_all_four_periods() {
        let today = day("2026-10-08");
        let rows: Vec<_> = [
            (16, "2026-10-08"),
            (15, "2026-10-08"),
            (14, "2026-10-08"),
            (13, "2026-10-08"),
            (12, "2026-10-08"),
            (11, "2026-10-08"),
            (10, "2026-10-07"),
            (9, "2026-10-06"),
            (8, "2026-10-01"),
            (7, "2026-10-01"),
            (6, "2026-10-01"),
            (5, "2026-09-30"),
            (4, "2026-09-29"),
            (3, "2026-09-28"),
            (2, "2026-09-01"),
            (1, "2025-12-31"),
        ]
        .into_iter()
        .map(|(id, date)| preview(id, date))
        .collect();
        let mut query = period_query();
        let closed = project_at(rows.clone(), &query, "epoch", 1, today).unwrap();
        assert_eq!(
            closed
                .recent
                .as_ref()
                .unwrap()
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            [16, 15, 14]
        );
        assert_eq!(
            closed
                .dates
                .iter()
                .map(|d| (d.key.as_str(), d.count))
                .collect::<Vec<_>>(),
            [("today", 3), ("week", 2), ("month", 3), ("older", 5)]
        );
        assert!(closed.date.is_empty() && closed.entries.is_empty());
        assert_eq!(closed.count, 0);
        let mut ids = vec![16, 15, 14];
        for key in PERIODS {
            query.date = Some(key.into());
            let page = project_at(rows.clone(), &query, "epoch", 1, today).unwrap();
            if key == "today" {
                assert_eq!(page.entries[0].members.len(), 3);
            }
            if key == "week" {
                assert_eq!(
                    page.entries.len(),
                    2,
                    "different local dates never merge into one repeat stack"
                );
            }
            ids.extend(
                page.entries
                    .iter()
                    .flat_map(|e| e.members.iter().map(|p| p.id)),
            );
        }
        ids.sort_unstable();
        assert_eq!(ids, (1..=16).collect::<Vec<_>>());
        let few = project_at(rows[..2].to_vec(), &period_query(), "epoch", 1, today).unwrap();
        assert_eq!(few.recent.unwrap().len(), 2);
        assert!(few.dates.iter().all(|d| d.count == 0));
    }

    #[test]
    fn periods_use_local_calendar_boundaries_with_week_precedence() {
        let classify =
            |created: &str, now: &str| period(preview(1, created).created_unix_ms, day(now));
        assert_eq!(classify("2026-10-08", "2026-10-08"), "today");
        assert_eq!(classify("2026-10-05", "2026-10-08"), "week");
        assert_eq!(classify("2026-10-04", "2026-10-08"), "month");
        assert_eq!(classify("2026-09-30", "2026-10-01"), "week");
        assert_eq!(classify("2026-11-01", "2026-11-02"), "month");
        assert_eq!(classify("2026-10-31", "2026-11-02"), "older");
        assert_eq!(classify("2026-12-31", "2027-01-01"), "week");
        assert_eq!(classify("2026-12-27", "2027-01-01"), "older");
        assert_eq!(classify("2026-10-25", "2026-10-26"), "month");
    }

    #[test]
    fn period_windows_are_bounded_and_midnight_invalidates_continuations() {
        let today = day("2026-10-08");
        let rows: Vec<_> = (1..=50).rev().map(|id| preview(id, "2026-10-08")).collect();
        let mut query = period_query();
        query.date = Some("today".into());
        query.group_similar = false;
        query.timeline_anchor = Some(format!("10:{}", rows[40].created_unix_ms));
        let first = project_at(rows.clone(), &query, "epoch", 1, today).unwrap();
        assert_eq!(first.entries.len(), WINDOW);
        assert_eq!(first.total_rows, 47);
        assert!(!first.anchor_reached);
        query.offset = first.next_offset.unwrap();
        query.period_day = Some(first.period_day);
        let next = project_at(rows.clone(), &query, "epoch", 1, today).unwrap();
        assert!(next.anchor_reached);
        assert_eq!(next.entries.len(), WINDOW);
        assert!(
            first
                .entries
                .iter()
                .all(|e| next.entries.iter().all(|n| n.key != e.key))
        );
        assert!(matches!(
            project_at(rows, &query, "epoch", 1, today.succ_opt().unwrap()),
            Err(HistoryError::Stale)
        ));
    }
}
