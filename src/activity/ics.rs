use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use super::{
    config::CalendarSourceConfig,
    model::ActivityEvent,
    provider::{CalendarProvider, ProviderFuture},
};

pub(crate) struct IcsProvider;

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
    events.sort_by(|a, b| {
        (a.start_unix_ms, a.end_unix_ms, &a.id).cmp(&(b.start_unix_ms, b.end_unix_ms, &b.id))
    });
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
    let mut current = EventBuilder::default();
    let mut components = Vec::<String>::new();
    let mut saw_calendar = false;
    for line in unfold_lines(contents) {
        if line.trim().is_empty() {
            continue;
        }
        let (key_and_params, raw_value) = line
            .split_once(':')
            .with_context(|| format!("invalid iCalendar content line in {}", path.display()))?;
        match key_and_params.to_ascii_uppercase().as_str() {
            "BEGIN" => {
                begin_component(&mut components, raw_value)?;
                saw_calendar = true;
            }
            "END" => {
                let component = raw_value.to_ascii_uppercase();
                anyhow::ensure!(
                    components.pop().as_deref() == Some(component.as_str()),
                    "mismatched END:{component} in {}",
                    path.display()
                );
                if component == "VEVENT" {
                    events.extend(finish_event(source, path, std::mem::take(&mut current))?);
                }
            }
            _ => {
                anyhow::ensure!(!components.is_empty(), "property outside VCALENDAR");
                // Nested components (notably VALARM) own their own properties.
                if components.last().map(String::as_str) == Some("VEVENT") {
                    parse_event_property(&mut current, key_and_params, raw_value)
                        .with_context(|| format!("parse event in {}", path.display()))?;
                }
            }
        }
    }
    anyhow::ensure!(saw_calendar, "missing VCALENDAR in {}", path.display());
    anyhow::ensure!(
        components.is_empty(),
        "truncated iCalendar file {}",
        path.display()
    );
    Ok(events)
}

fn begin_component(components: &mut Vec<String>, value: &str) -> Result<()> {
    let component = value.to_ascii_uppercase();
    match components.last().map(String::as_str) {
        None => anyhow::ensure!(component == "VCALENDAR", "expected BEGIN:VCALENDAR"),
        Some(parent) => {
            anyhow::ensure!(component != "VCALENDAR", "nested VCALENDAR is invalid");
            anyhow::ensure!(
                component != "VEVENT" || parent == "VCALENDAR",
                "VEVENT must belong directly to VCALENDAR"
            );
        }
    }
    anyhow::ensure!(!component.is_empty(), "empty iCalendar component");
    components.push(component);
    Ok(())
}

fn parse_event_property(
    builder: &mut EventBuilder,
    key_and_params: &str,
    value: &str,
) -> Result<()> {
    let key = key_and_params
        .split(';')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match key.as_str() {
        "UID" => builder.uid = decode_text(value),
        "SUMMARY" => builder.title = decode_text(value),
        "DTSTART" => builder.start = Some(parse_time(key_and_params, value)?),
        "DTEND" => builder.end = Some(parse_time(key_and_params, value)?),
        "LOCATION" => builder.location = decode_text(value),
        "URL" => builder.url = decode_text(value),
        "STATUS" => builder.cancelled = value.eq_ignore_ascii_case("CANCELLED"),
        _ => {}
    }
    Ok(())
}

fn finish_event(
    source: &CalendarSourceConfig,
    path: &Path,
    builder: EventBuilder,
) -> Result<Option<ActivityEvent>> {
    if builder.cancelled {
        return Ok(None);
    }
    let start = builder.start.context("VEVENT is missing DTSTART")?;
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
        timezone: None, // Only DTSTART's timezone is published.
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
    anyhow::ensure!(end.unix_ms >= start.unix_ms, "DTEND precedes DTSTART");
    Ok(Some(ActivityEvent {
        id: format!("{}:{uid}:{}", source.id, start.unix_ms),
        source_id: source.id.clone(),
        calendar_name: source.display_name().into(),
        color: source.color.clone(),
        title,
        start_unix_ms: start.unix_ms,
        end_unix_ms: end.unix_ms,
        all_day: start.all_day,
        start_date: start.date,
        end_date: end.date,
        timezone: start.timezone,
        location: builder.location,
        url: builder.url,
    }))
}

fn parse_time(key_and_params: &str, value: &str) -> Result<ParsedTime> {
    let params = key_and_params
        .split(';')
        .skip(1)
        .filter_map(|part| part.split_once('='))
        .collect::<Vec<_>>();
    let timezone = params
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("TZID"))
        .map(|(_, value)| value.trim_matches('"').to_string());
    let is_date = params.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("VALUE") && value.eq_ignore_ascii_case("DATE")
    }) || (!value.contains('T') && value.len() == 8);
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
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(previous) = lines.last_mut()
        {
            previous.push_str(&line[1..]);
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

    use super::parse_calendar;
    use crate::activity::config::CalendarSourceConfig;

    fn source() -> CalendarSourceConfig {
        CalendarSourceConfig {
            id: "work".into(),
            name: "Work".into(),
            color: "#123456".into(),
            ..CalendarSourceConfig::default()
        }
    }

    #[test]
    fn nested_alarm_and_adjacent_events_keep_their_own_properties() {
        let events = parse_calendar(&source(), Path::new("test.ics"),
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:meeting\nSUMMARY:Team\n  meeting\nDTSTART:20260115T090000Z\nBEGIN:VALARM\nACTION:EMAIL\nTRIGGER:-PT15M\nSUMMARY:Reminder\nDESCRIPTION:Meeting soon\nATTENDEE:mailto:person@example.com\nEND:VALARM\nEND:VEVENT\nBEGIN:VEVENT\nSTATUS:CANCELLED\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART;VALUE=DATE:20260116\nEND:VEVENT\nEND:VCALENDAR\n").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Team meeting");
        assert!(events[0].id.contains(":meeting:"));
        assert_eq!(events[1].title, "Untitled event");
        assert_eq!(events[1].start_date.as_deref(), Some("2026-01-16"));
        assert_eq!(events[1].end_date.as_deref(), Some("2026-01-17"));
        assert_eq!(events[1].end_unix_ms - events[1].start_unix_ms, 86_400_000);
        assert_eq!(events[1].timezone, None);
    }

    #[test]
    fn rejects_malformed_calendars_but_accepts_an_empty_calendar() {
        for contents in [
            "",
            "not a calendar",
            "BEGIN:VEVENT\nEND:VEVENT\n",
            "BEGIN:VCALENDAR\nBEGIN:VCALENDAR\nEND:VCALENDAR\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nBEGIN:VEVENT\nEND:VEVENT\nEND:VEVENT\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:\nEND:\nEND:VCALENDAR\n",
            "SUMMARY:outside\nBEGIN:VCALENDAR\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20260115T090000Z\n",
            "BEGIN:VCALENDAR\nEND:VEVENT\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:Missing start\nEND:VEVENT\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:invalid\nEND:VEVENT\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20260115T090000Z\nDTEND:invalid\nEND:VEVENT\nEND:VCALENDAR\n",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20260115T090000Z\nBEGIN:VALARM\nEND:VEVENT\nEND:VCALENDAR\n",
        ] {
            assert!(
                parse_calendar(&source(), Path::new("test.ics"), contents).is_err(),
                "{contents}"
            );
        }
        assert!(
            parse_calendar(
                &source(),
                Path::new("test.ics"),
                "BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR\n"
            )
            .unwrap()
            .is_empty()
        );
    }
}
