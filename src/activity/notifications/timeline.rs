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

#[cfg(test)]
mod tests {
    use super::super::center::{CenterQuery, Preview, PreviewHints, repeat_key};
    use super::super::history::HistoryError;
    use super::{WINDOW, project_at};
    use chrono::{Local, NaiveDate, TimeZone};

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
            repeat_key: repeat_key("Repeated", "Body", "", 0, true),
        }
    }
    fn period_query() -> CenterQuery {
        serde_json::from_value(
            serde_json::json!({"view":"timeline", "app_key":"named:Chat", "period_groups":true}),
        )
        .unwrap()
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
