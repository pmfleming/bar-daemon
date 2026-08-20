use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use super::{
    config::CalendarSourceConfig,
    model::ActivityEvent,
    provider::{CalendarProvider, ProviderFuture},
};

pub struct IcsProvider;

impl CalendarProvider for IcsProvider {
    fn kinds(&self) -> &'static [&'static str] {
        &["ics-file", "ics-directory"]
    }

    fn load<'a>(&'a self, source: &'a CalendarSourceConfig) -> ProviderFuture<'a> {
        Box::pin(load_source(source))
    }
}

#[derive(Debug)]
struct ParsedTime {
    unix_ms: i64,
    date: Option<String>,
    all_day: bool,
    timezone: Option<String>,
}

#[derive(Default)]
struct EventBuilder {
    uid: String,
    title: String,
    start: Option<ParsedTime>,
    end: Option<ParsedTime>,
    location: String,
    url: String,
    cancelled: bool,
}

async fn load_source(source: &CalendarSourceConfig) -> Result<Vec<ActivityEvent>> {
    let source = source.clone();
    tokio::task::spawn_blocking(move || load_source_blocking(&source))
        .await
        .context("join iCalendar source loader")?
}

fn load_source_blocking(source: &CalendarSourceConfig) -> Result<Vec<ActivityEvent>> {
    if source.id.trim().is_empty() {
        bail!("calendar source id cannot be empty");
    }
    let mut files = Vec::new();
    match source.kind.as_str() {
        "ics-file" => files.push(source.path.clone()),
        "ics-directory" => collect_ics_files(&source.path, &mut files)?,
        kind => bail!("unsupported calendar source kind {kind}"),
    }
    files.sort();

    let mut events = Vec::new();
    for path in files {
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("read iCalendar file {}", path.display()))?;
        events.extend(parse_calendar(source, &path, &contents)?);
    }
    events.sort_by_key(|event| (event.start_unix_ms, event.end_unix_ms, event.id.clone()));
    Ok(events)
}

fn collect_ics_files(path: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("read calendar directory {}", path.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_ics_files(&entry.path(), files)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ics"))
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn parse_calendar(
    source: &CalendarSourceConfig,
    path: &Path,
    contents: &str,
) -> Result<Vec<ActivityEvent>> {
    let mut events = Vec::new();
    let mut current: Option<EventBuilder> = None;
    for line in unfold_lines(contents) {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VEVENT" {
            current = Some(EventBuilder::default());
            continue;
        }
        if upper == "END:VEVENT" {
            if let Some(builder) = current.take()
                && let Some(event) = finish_event(source, path, builder)
            {
                events.push(event);
            }
            continue;
        }
        let Some(builder) = current.as_mut() else {
            continue;
        };
        let Some((key_and_params, raw_value)) = line.split_once(':') else {
            continue;
        };
        let key = key_and_params
            .split(';')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        match key.as_str() {
            "UID" => builder.uid = decode_text(raw_value),
            "SUMMARY" => builder.title = decode_text(raw_value),
            "DTSTART" => builder.start = parse_time(key_and_params, raw_value).ok(),
            "DTEND" => builder.end = parse_time(key_and_params, raw_value).ok(),
            "LOCATION" => builder.location = decode_text(raw_value),
            "URL" => builder.url = decode_text(raw_value),
            "STATUS" => builder.cancelled = raw_value.eq_ignore_ascii_case("CANCELLED"),
            _ => {}
        }
    }
    Ok(events)
}

fn finish_event(
    source: &CalendarSourceConfig,
    path: &Path,
    builder: EventBuilder,
) -> Option<ActivityEvent> {
    if builder.cancelled {
        return None;
    }
    let start = builder.start?;
    let default_duration = if start.all_day { 86_400_000 } else { 3_600_000 };
    let end = builder.end.unwrap_or_else(|| ParsedTime {
        unix_ms: start.unix_ms + default_duration,
        date: start.date.as_deref().and_then(|date| {
            NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.succ_opt())
                .map(|date| date.format("%Y-%m-%d").to_string())
        }),
        all_day: start.all_day,
        timezone: start.timezone.clone(),
    });
    let uid = if builder.uid.is_empty() {
        format!("{}-{}", path.display(), start.unix_ms)
    } else {
        builder.uid
    };
    let title = if builder.title.trim().is_empty() {
        "Untitled event".into()
    } else {
        builder.title
    };
    Some(ActivityEvent {
        id: format!("{}:{uid}:{}", source.id, start.unix_ms),
        source_id: source.id.clone(),
        calendar_name: if source.name.is_empty() {
            source.id.clone()
        } else {
            source.name.clone()
        },
        color: source.color.clone(),
        title,
        start_unix_ms: start.unix_ms,
        end_unix_ms: end.unix_ms.max(start.unix_ms),
        all_day: start.all_day,
        start_date: start.date,
        end_date: end.date,
        timezone: start.timezone,
        location: builder.location,
        url: builder.url,
    })
}

fn parse_time(key_and_params: &str, value: &str) -> Result<ParsedTime> {
    let params = key_and_params
        .split(';')
        .skip(1)
        .filter_map(|part| part.split_once('='))
        .map(|(key, value)| (key.to_ascii_uppercase(), value.to_string()))
        .collect::<Vec<_>>();
    let timezone = params
        .iter()
        .find(|(key, _)| key == "TZID")
        .map(|(_, value)| value.trim_matches('"').to_string());
    let is_date = params
        .iter()
        .any(|(key, value)| key == "VALUE" && value.eq_ignore_ascii_case("DATE"))
        || (!value.contains('T') && value.len() == 8);
    if is_date {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d")
            .with_context(|| format!("parse iCalendar date {value}"))?;
        let datetime = date.and_hms_opt(0, 0, 0).context("construct midnight")?;
        return Ok(ParsedTime {
            unix_ms: datetime.and_utc().timestamp_millis(),
            date: Some(date.format("%Y-%m-%d").to_string()),
            all_day: true,
            timezone,
        });
    }

    if let Some(value) = value.strip_suffix('Z') {
        let datetime = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")?;
        return Ok(ParsedTime {
            unix_ms: datetime.and_utc().timestamp_millis(),
            date: None,
            all_day: false,
            timezone: Some("UTC".into()),
        });
    }

    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M"))?;
    let unix_ms = if let Some(name) = timezone.as_deref() {
        let zone: Tz = name
            .parse()
            .with_context(|| format!("parse timezone {name}"))?;
        zone.from_local_datetime(&naive)
            .earliest()
            .context("local calendar time does not exist")?
            .with_timezone(&Utc)
            .timestamp_millis()
    } else {
        Local
            .from_local_datetime(&naive)
            .earliest()
            .context("local calendar time does not exist")?
            .with_timezone(&Utc)
            .timestamp_millis()
    };
    Ok(ParsedTime {
        unix_ms,
        date: None,
        all_day: false,
        timezone,
    })
}

fn unfold_lines(contents: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&line[1..]);
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn decode_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::activity::config::CalendarSourceConfig;

    use super::{parse_calendar, unfold_lines};

    fn source() -> CalendarSourceConfig {
        CalendarSourceConfig {
            id: "work".into(),
            name: "Work".into(),
            color: "#123456".into(),
            ..CalendarSourceConfig::default()
        }
    }

    #[test]
    fn parses_timed_and_all_day_events() {
        let events = parse_calendar(
            &source(),
            Path::new("test.ics"),
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:meeting\nSUMMARY:Planning\\, weekly\nDTSTART;TZID=Europe/Amsterdam:20260115T090000\nDTEND;TZID=Europe/Amsterdam:20260115T100000\nLOCATION:Room 1\nEND:VEVENT\nBEGIN:VEVENT\nUID:holiday\nSUMMARY:Holiday\nDTSTART;VALUE=DATE:20260120\nDTEND;VALUE=DATE:20260121\nEND:VEVENT\nEND:VCALENDAR\n",
        )
        .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Planning, weekly");
        assert_eq!(events[0].timezone.as_deref(), Some("Europe/Amsterdam"));
        assert_eq!(events[0].location, "Room 1");
        assert!(events[1].all_day);
        assert_eq!(events[1].start_date.as_deref(), Some("2026-01-20"));
    }

    #[test]
    fn unfolds_continuation_lines() {
        assert_eq!(
            unfold_lines("SUMMARY:Long\n title\nUID:1"),
            vec!["SUMMARY:Longtitle", "UID:1"]
        );
    }
}
