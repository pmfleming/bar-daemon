//! Local-calendar search over retained runs, before pagination.
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, NaiveDate, TimeZone};

#[derive(Debug, Hash)]
enum Day {
    On(NaiveDate),
    Since(NaiveDate),
    Before(NaiveDate),
}
#[derive(Debug, Hash)]
pub(super) struct Search {
    words: Vec<String>,
    days: Vec<Day>,
}
impl Search {
    pub fn parse<T: TimeZone>(text: &str, now: &DateTime<T>) -> Result<Self> {
        let mut search = Self {
            words: vec![],
            days: vec![],
        };
        for word in text.to_lowercase().split_whitespace() {
            let (kind, value) = word.split_once(':').unwrap_or(("", word));
            let relative = match value {
                "today" => Some(now.date_naive()),
                "yesterday" => now.date_naive().pred_opt(),
                _ => None,
            };
            let iso = value.len() == 10
                && value.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                && value.as_bytes()[4] == b'-'
                && value.as_bytes()[7] == b'-';
            if ["on", "since", "before"].contains(&kind)
                || (kind.is_empty() && (relative.is_some() || iso))
            {
                let date = if let Some(date) = relative {
                    date
                } else {
                    ensure!(iso, "Use YYYY-MM-DD, today or yesterday for date searches");
                    NaiveDate::parse_from_str(value, "%Y-%m-%d").with_context(|| {
                        format!("Invalid history date {value:?}; use YYYY-MM-DD")
                    })?
                };
                search.days.push(match kind {
                    "since" => Day::Since(date),
                    "before" => Day::Before(date),
                    _ => Day::On(date),
                });
            } else {
                search.words.push(word.to_owned());
            }
        }
        Ok(search)
    }
    pub fn matches<T: TimeZone>(&self, text: &str, started_at: u64, timezone: &T) -> bool {
        if !self.words.iter().all(|word| text.contains(word)) {
            return false;
        }
        if self.days.is_empty() {
            return true;
        }
        let date = i64::try_from(started_at)
            .ok()
            .and_then(|at| DateTime::from_timestamp(at, 0))
            .map(|at| at.with_timezone(timezone).date_naive());
        self.days.iter().all(|day| {
            date.is_some_and(|date| match day {
                Day::On(at) => date == *at,
                Day::Since(at) => date >= *at,
                Day::Before(at) => date < *at,
            })
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Amsterdam;
    #[test]
    fn relative_search_hashes_change_at_midnight_but_fixed_dates_do_not() {
        use std::hash::{Hash, Hasher};
        let before = Amsterdam.with_ymd_and_hms(2026, 10, 24, 23, 59, 0).unwrap();
        let after = Amsterdam.with_ymd_and_hms(2026, 10, 25, 0, 1, 0).unwrap();
        let hash = |text, now| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            Search::parse(text, now).unwrap().hash(&mut hasher);
            hasher.finish()
        };
        assert_ne!(hash("today", &before), hash("today", &after));
        assert_eq!(
            hash("on:2026-10-24", &before),
            hash("on:2026-10-24", &after)
        );
        assert!(
            Search::parse("word-ab-cd", &before)
                .unwrap()
                .matches("word-ab-cd", 1, &Amsterdam)
        );
    }
    #[test]
    fn local_calendar_dates_cover_dst_leap_days_and_combined_terms() {
        let now = Amsterdam.with_ymd_and_hms(2026, 3, 30, 12, 0, 0).unwrap();
        let search = Search::parse("FAILED since:2026-03-29 before:today", &now).unwrap();
        for hour in [0, 23] {
            let at = Amsterdam
                .with_ymd_and_hms(2026, 3, 29, hour, 30, 0)
                .unwrap()
                .timestamp() as u64;
            assert!(search.matches("failed pi", at, &Amsterdam));
            assert!(!search.matches("current pi", at, &Amsterdam));
            assert!(
                Search::parse("yesterday", &now)
                    .unwrap()
                    .matches("", at, &Amsterdam)
            );
        }
        assert!(!search.matches("failed", now.timestamp() as u64, &Amsterdam));
        assert!(Search::parse("on:2024-02-29", &now).is_ok());
        for text in [
            "2025-02-29",
            "on:2026-13-01",
            "since:garbage",
            "before:2026-2-1",
        ] {
            assert!(Search::parse(text, &now).is_err(), "{text}");
        }
        let fall = Amsterdam.with_ymd_and_hms(2026, 10, 25, 2, 30, 0);
        let search = Search::parse("2026-10-25", &now).unwrap();
        for at in [fall.earliest().unwrap(), fall.latest().unwrap()] {
            assert!(search.matches("", at.timestamp() as u64, &Amsterdam));
        }
    }
}
