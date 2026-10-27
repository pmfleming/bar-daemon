//! Civil-date membership is shared by catalog scope and calendar markers.
use super::agenda_store::model::{Schedule, ScheduleMode};
use anyhow::{Context, Result, ensure};
use chrono::{Days, NaiveDate, TimeZone};

pub(crate) fn date(value: &str) -> Result<NaiveDate> {
    let day = NaiveDate::parse_from_str(value, "%Y-%m-%d").context("expected YYYY-MM-DD")?;
    ensure!(day.to_string() == value, "expected YYYY-MM-DD");
    Ok(day)
}

#[derive(Clone, Copy)]
pub(crate) enum Membership {
    Civil(NaiveDate, NaiveDate),
    Timed(i64, i64),
    Instant(i64),
}
impl Membership {
    pub fn includes(self, day: NaiveDate, bounds: Option<(i64, i64)>) -> bool {
        match self {
            Self::Civil(s, e) => s <= day && day <= e,
            Self::Timed(s, e) => bounds.is_some_and(|(a, b)| a < b && s < e && s < b && e > a),
            Self::Instant(s) => bounds.is_some_and(|(a, b)| a <= s && s < b),
        }
    }
}
pub(crate) struct Projection<T: TimeZone> {
    pub today: NaiveDate,
    pub zone: T,
}
impl<T: TimeZone> Projection<T> {
    pub(super) fn midnight(&self, day: NaiveDate) -> Option<i64> {
        // Some zones skip midnight (or even an entire civil day). Use the first
        // valid local instant, never a fixed 24-hour offset across DST.
        (0..3).find_map(|offset| {
            let day = day.checked_add_days(Days::new(offset))?;
            (0..86400).find_map(|second| {
                let time = day.and_hms_opt(second / 3600, (second % 3600) / 60, second % 60)?;
                self.zone
                    .from_local_datetime(&time)
                    .earliest()
                    .map(|t| t.timestamp_millis())
            })
        })
    }
    pub(crate) fn bounds(&self, day: NaiveDate) -> Option<(i64, i64)> {
        self.midnight(day)
            .zip(day.succ_opt().and_then(|d| self.midnight(d)))
    }
    pub(crate) fn includes(&self, schedule: &Schedule, day: NaiveDate) -> bool {
        self.membership(schedule)
            .is_some_and(|m| m.includes(day, self.bounds(day)))
    }
    pub(crate) fn membership(&self, schedule: &Schedule) -> Option<Membership> {
        match schedule.mode {
            ScheduleMode::Date => {
                let start = schedule.start_date.as_deref().and_then(|d| date(d).ok());
                let end = schedule.end_date.as_deref().and_then(|d| date(d).ok());
                let range = match (start, end) {
                    (Some(s), Some(e)) if s <= e => Some((s, e)),
                    (None, Some(e)) if e >= self.today => Some((self.today, e)),
                    (Some(s), None) if s <= self.today => Some((s, self.today)),
                    _ => None,
                };
                range.map(|(s, e)| Membership::Civil(s, e))
            }
            ScheduleMode::Timed => {
                let local_date = |ms| {
                    self.zone
                        .timestamp_millis_opt(ms)
                        .single()
                        .map(|t| t.date_naive())
                };
                let range = match (schedule.start_unix_ms, schedule.end_unix_ms) {
                    (Some(s), Some(e)) if s <= e => Some((s, e)),
                    (None, Some(e)) if local_date(e).is_some_and(|d| d >= self.today) => {
                        self.midnight(self.today).map(|s| (s, e))
                    }
                    (Some(s), None) if local_date(s).is_some_and(|d| d <= self.today) => self
                        .today
                        .checked_add_days(Days::new(1))
                        .and_then(|d| self.midnight(d))
                        .map(|e| (s, e)),
                    _ => None,
                };
                let (s, e) = range?;
                // Imported zero-duration VEVENTs are preserved as instants.
                if s == e && schedule.start_unix_ms.is_some() && schedule.end_unix_ms.is_some() {
                    Some(Membership::Instant(s))
                } else if s < e {
                    Some(Membership::Timed(s, e))
                } else {
                    None
                }
            }
        }
    }
    pub(crate) fn stored_dates(&self, s: &Schedule) -> (Option<NaiveDate>, Option<NaiveDate>) {
        match s.mode {
            ScheduleMode::Date => (
                s.start_date.as_deref().and_then(|s| date(s).ok()),
                s.end_date.as_deref().and_then(|s| date(s).ok()),
            ),
            ScheduleMode::Timed => {
                let convert = |ms| {
                    self.zone
                        .timestamp_millis_opt(ms)
                        .single()
                        .map(|t| t.date_naive())
                };
                (
                    s.start_unix_ms.and_then(convert),
                    s.end_unix_ms.and_then(convert),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn civil(start: Option<&str>, end: Option<&str>) -> Schedule {
        Schedule {
            start_date: start.map(str::to_owned),
            end_date: end.map(str::to_owned),
            ..Schedule::default()
        }
    }
    #[test]
    fn civil_endpoint_matrix_rejects_reversed_ranges() {
        let p = Projection {
            today: date("2026-10-27").unwrap(),
            zone: chrono_tz::UTC,
        };
        for (s, e, days) in [
            (Some("2026-10-26"), Some("2026-10-28"), vec![26, 27, 28]),
            (None, Some("2026-10-28"), vec![27, 28]),
            (Some("2026-10-26"), None, vec![26, 27]),
            (None, Some("2026-10-26"), vec![]),
            (Some("2026-10-28"), None, vec![]),
            (None, None, vec![]),
            (Some("2026-10-28"), Some("2026-10-26"), vec![]),
        ] {
            let schedule = civil(s, e);
            let actual: Vec<_> = (25..=29)
                .filter(|n| p.includes(&schedule, date(&format!("2026-10-{n}")).unwrap()))
                .collect();
            assert_eq!(actual, days, "{s:?} {e:?}");
        }
    }
    #[test]
    fn skipped_civil_day_has_no_timed_membership_but_civil_dates_stay_fixed() {
        let p = Projection {
            today: date("2011-12-29").unwrap(),
            zone: chrono_tz::Pacific::Apia,
        };
        let ms = |s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .timestamp_millis()
        };
        let timed = Schedule {
            mode: ScheduleMode::Timed,
            start_unix_ms: Some(ms("2011-12-29T10:00:00Z")),
            end_unix_ms: Some(ms("2011-12-31T10:00:00Z")),
            timezone: Some("Pacific/Apia".into()),
            ..Schedule::default()
        };
        assert!(p.includes(&timed, date("2011-12-29").unwrap()));
        assert!(!p.includes(&timed, date("2011-12-30").unwrap()));
        assert!(p.includes(&timed, date("2011-12-31").unwrap()));
        assert!(p.includes(
            &civil(Some("2011-12-29"), Some("2011-12-31")),
            date("2011-12-30").unwrap()
        ));
    }
    #[test]
    fn timed_midnight_and_dst_inferred_boundaries() {
        use chrono::DateTime;
        let ms = |s| DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        for (today, start, end) in [
            (
                "2026-03-29",
                "2026-03-28T23:00:00+01:00",
                "2026-03-29T00:00:00+01:00",
            ),
            (
                "2026-10-25",
                "2026-10-24T23:00:00+02:00",
                "2026-10-25T00:00:00+02:00",
            ),
        ] {
            let today = date(today).unwrap();
            let p = Projection {
                today,
                zone: chrono_tz::Europe::Berlin,
            };
            let mut s = Schedule {
                mode: ScheduleMode::Timed,
                start_unix_ms: Some(ms(start)),
                end_unix_ms: Some(ms(end)),
                timezone: Some("Europe/Berlin".into()),
                ..Schedule::default()
            };
            assert!(p.includes(&s, today.pred_opt().unwrap()));
            assert!(!p.includes(&s, today));
            s.start_unix_ms = None;
            assert!(!p.includes(&s, today), "end-only at midnight is empty");
            s.start_unix_ms = Some(ms(start));
            s.end_unix_ms = None;
            assert!(p.includes(&s, today));
            assert!(!p.includes(&s, today.succ_opt().unwrap()));
        }
    }
}
