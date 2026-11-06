//! Compatibility at the import/legacy boundaries, not a second record owner.
use super::{
    agenda_projection::date,
    agenda_store::model::*,
    model::{ActivityEvent, TodoItem},
};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

fn source_uid(event: &ActivityEvent) -> Option<String> {
    event
        .id
        .strip_prefix(&format!("{}:", event.source_id))
        .and_then(|s| s.rsplit_once(':').map(|(uid, _)| uid.to_owned()))
}
pub(super) fn imported_id(event: &ActivityEvent) -> String {
    // A non-recurring imported identity is source + UID, never DTSTART. Moving
    // the same UID must not delete/recreate the selected read-only item.
    let identity = (
        event.source_id.as_str(),
        source_uid(event).unwrap_or_else(|| event.id.clone()),
    );
    format!(
        "ics:{:x}",
        Sha256::digest(serde_json::to_vec(&identity).expect("source identity serialization"))
    )
}
pub(super) fn imported(event: &ActivityEvent) -> Result<Record> {
    let schedule = if event.all_day {
        // ActivityEvent retains the ICS convention for its legacy consumers.
        // Convert exclusive DTEND exactly once as it enters the agenda model.
        Schedule {
            mode: ScheduleMode::Date,
            start_date: event.start_date.clone(),
            end_date: Some(
                date(event.end_date.as_deref().context("missing ICS DTEND")?)?
                    .pred_opt()
                    .context("ICS end underflow")?
                    .to_string(),
            ),
            ..Schedule::default()
        }
    } else {
        Schedule {
            mode: ScheduleMode::Timed,
            start_unix_ms: Some(event.start_unix_ms),
            end_unix_ms: Some(event.end_unix_ms),
            timezone: Some(event.timezone.clone().unwrap_or_else(|| "UTC".into())),
            ..Schedule::default()
        }
    };
    schedule.validate(true)?;
    let record = Record {
        id: imported_id(event),
        kind: ItemKind::Event,
        source_id: event.source_id.clone(),
        source_uid: source_uid(event),
        revision: 1,
        title: event.title.clone(),
        notes: String::new(),
        links: if event.url.is_empty() {
            vec![]
        } else {
            vec![Link {
                id: "source-url".into(),
                label: String::new(),
                url: event.url.clone(),
            }]
        },
        schedule,
        created_unix_ms: 0,
        modified_unix_ms: 0,
        capabilities: Capabilities::default(),
        event: Some(EventDetails {
            location: event.location.clone(),
            ..EventDetails::default()
        }),
        task: None,
    };
    record.validate()?;
    Ok(record)
}
pub(super) fn legacy_event<T: chrono::TimeZone>(
    record: &Record,
    p: &super::agenda_projection::Projection<T>,
) -> Option<ActivityEvent> {
    let details = record.event.as_ref()?;
    let s = &record.schedule;
    let (start, end, start_date, end_date) = match s.mode {
        ScheduleMode::Date => {
            let start = s
                .start_date
                .as_deref()
                .and_then(|d| date(d).ok())
                .unwrap_or(p.today);
            let end = s
                .end_date
                .as_deref()
                .and_then(|d| date(d).ok())
                .unwrap_or(p.today)
                .succ_opt()?;
            (
                p.midnight(start)?,
                p.midnight(end)?,
                Some(start.to_string()),
                Some(end.to_string()),
            )
        }
        ScheduleMode::Timed => (
            s.start_unix_ms.or_else(|| p.midnight(p.today))?,
            s.end_unix_ms
                .or_else(|| p.today.succ_opt().and_then(|d| p.midnight(d)))?,
            None,
            None,
        ),
    };
    Some(ActivityEvent {
        id: record.id.clone(),
        source_id: record.source_id.clone(),
        title: record.title.clone(),
        calendar_name: record.source_id.clone(),
        start_unix_ms: start,
        end_unix_ms: end,
        all_day: s.mode == ScheduleMode::Date,
        start_date,
        end_date,
        timezone: s.timezone.clone(),
        location: details.location.clone(),
        url: record
            .links
            .first()
            .map(|l| l.url.clone())
            .unwrap_or_default(),
        ..ActivityEvent::default()
    })
}
pub(super) fn todo(record: &Record) -> Option<TodoItem> {
    let task = record.task.as_ref()?;
    Some(TodoItem {
        id: record.id.clone(),
        source_id: record.source_id.clone(),
        title: record.title.clone(),
        completed: task.completed,
        priority: task.priority.try_into().unwrap_or(u8::MAX),
        due_date: record.schedule.end_date.clone(),
        due_unix_ms: record.schedule.end_unix_ms,
        created_unix_ms: record.created_unix_ms,
        completed_unix_ms: task.completed_unix_ms,
    })
}
