//! Validate editor wall-time text natively before reducing it to instants.
use super::{
    agenda_projection::date,
    agenda_store::model::{Schedule, ScheduleMode},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Offset};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub mode: ScheduleMode,
    pub start_text: String,
    pub end_text: String,
    pub timezone: String,
}
pub(crate) fn parse(request: Request) -> Result<Schedule> {
    ensure!(
        request.start_text.len() <= 128
            && request.end_text.len() <= 128
            && request.timezone.len() <= 128,
        "schedule text too long"
    );
    let start = request.start_text.trim();
    let end = request.end_text.trim();
    let schedule = match request.mode {
        ScheduleMode::Date => {
            let civil = |s: &str| -> Result<Option<String>> {
                if s.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(date(s)?.to_string()))
                }
            };
            Schedule {
                start_date: civil(start)?,
                end_date: civil(end)?,
                ..Schedule::default()
            }
        }
        ScheduleMode::Timed => {
            let zone: chrono_tz::Tz = request
                .timezone
                .parse()
                .context("timed schedule requires IANA timezone")?;
            let instant = |s: &str| -> Result<Option<i64>> {
                if s.is_empty() {
                    return Ok(None);
                }
                let supplied = DateTime::parse_from_rfc3339(s)
                    .context("use RFC3339 date/time with explicit chosen offset")?;
                let actual = supplied.with_timezone(&zone);
                ensure!(
                    actual.naive_local() == supplied.naive_local()
                        && actual.offset().fix() == *supplied.offset(),
                    "local time/offset is invalid for {zone}; correct DST gap or choose the matching offset"
                );
                Ok(Some(supplied.timestamp_millis()))
            };
            Schedule {
                mode: ScheduleMode::Timed,
                start_unix_ms: instant(start)?,
                end_unix_ms: instant(end)?,
                timezone: Some(zone.name().into()),
                ..Schedule::default()
            }
        }
    };
    schedule.validate(false)?;
    Ok(schedule)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offsets_are_chosen_and_verified_not_normalized() {
        let parse = |s: &str| {
            super::parse(Request {
                mode: ScheduleMode::Timed,
                start_text: s.into(),
                end_text: String::new(),
                timezone: "Europe/Berlin".into(),
            })
        };
        for s in [
            "2026-03-29T02:30:00+01:00",
            "2026-03-29T02:30:00+02:00",
            "2026-02-30T10:00:00+01:00",
            "2026-10-25T02:30:00",
            "2026-10-25T02:30:00+00:00",
        ] {
            assert!(parse(s).is_err(), "{s}");
        }
        let a = parse("2026-10-25T02:30:00+02:00").unwrap();
        let b = parse("2026-10-25T02:30:00+01:00").unwrap();
        assert_eq!(
            b.start_unix_ms.unwrap() - a.start_unix_ms.unwrap(),
            3_600_000
        );
        assert!(parse("2026-03-29T03:30:00+02:00").is_ok());
    }
}
