//! Local-calendar search over retained runs, before pagination.
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, NaiveDate, TimeZone};

#[derive(Hash)]
enum Day {
    On(NaiveDate),
    Since(NaiveDate),
    Before(NaiveDate),
}
impl Day {
    fn parse(word: &str, today: NaiveDate) -> Result<Option<Self>> {
        let (kind, value) = word.split_once(':').unwrap_or(("", word));
        let iso = value.len() == 10
            && value.as_bytes()[..4].iter().all(u8::is_ascii_digit)
            && value.as_bytes()[4] == b'-'
            && value.as_bytes()[7] == b'-';
        let bare_date = kind.is_empty() && (matches!(value, "today" | "yesterday") || iso);
        if !bare_date && !["on", "since", "before"].contains(&kind) {
            return Ok(None);
        }
        let date = match value {
            "today" => today,
            "yesterday" => today
                .pred_opt()
                .context("Yesterday is outside the supported calendar")?,
            _ => {
                ensure!(iso, "Use YYYY-MM-DD, today or yesterday for date searches");
                NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .with_context(|| format!("Invalid history date {value:?}; use YYYY-MM-DD"))?
            }
        };
        Ok(Some(match kind {
            "since" => Self::Since(date),
            "before" => Self::Before(date),
            _ => Self::On(date),
        }))
    }

    fn matches(&self, date: NaiveDate) -> bool {
        match self {
            Self::On(at) => date == *at,
            Self::Since(at) => date >= *at,
            Self::Before(at) => date < *at,
        }
    }
}
#[derive(Hash)]
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
            if let Some(day) = Day::parse(word, now.date_naive())? {
                search.days.push(day);
            } else {
                search.words.push(word.to_owned());
            }
        }
        Ok(search)
    }
    pub fn matches_with<T: TimeZone>(
        &self,
        started_at: u64,
        timezone: &T,
        text: impl FnOnce() -> String,
    ) -> bool {
        // Date filters are cheap and don't need a serialized component payload.
        // In particular, ordinary unfiltered browsing needs no search text at all.
        if !self.days.is_empty() {
            let Some(date) = i64::try_from(started_at)
                .ok()
                .and_then(|at| DateTime::from_timestamp(at, 0))
                .map(|at| at.with_timezone(timezone).date_naive())
            else {
                return false;
            };
            if !self.days.iter().all(|day| day.matches(date)) {
                return false;
            }
        }
        if self.words.is_empty() {
            return true;
        }
        let text = text();
        self.words.iter().all(|word| text.contains(word))
    }

    #[cfg(test)]
    fn matches<T: TimeZone>(&self, text: &str, started_at: u64, timezone: &T) -> bool {
        self.matches_with(started_at, timezone, || text.to_owned())
    }
}
#[cfg(test)]
mod tests {
    use super::Search;
    use chrono::TimeZone;
    use chrono_tz::Europe::Amsterdam;
    #[test]
    fn builds_search_text_only_for_text_terms_that_survive_dates() {
        let now = Amsterdam.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap();
        let at = now.timestamp() as u64;
        for (query, expected) in [
            ("", true),
            ("today", true),
            ("before:today", false),
            ("failed before:today", false),
        ] {
            assert_eq!(
                Search::parse(query, &now)
                    .unwrap()
                    .matches_with(at, &Amsterdam, || panic!(
                        "unnecessary text construction: {query}"
                    )),
                expected
            );
        }
        let calls = std::cell::Cell::new(0);
        assert!(
            Search::parse("failed today component", &now)
                .unwrap()
                .matches_with(at, &Amsterdam, || {
                    calls.set(calls.get() + 1);
                    "failed component".into()
                })
        );
        assert_eq!(calls.get(), 1);
    }

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
            "on:",
            "on:ééééé",
            "since:yesterday:extra",
        ] {
            assert!(Search::parse(text, &now).is_err(), "{text}");
        }
        for word in ["ÉCLAIR", "unknown:today", "word-ab-cd"] {
            assert!(Search::parse(word, &now).unwrap().matches(
                &word.to_lowercase(),
                0,
                &Amsterdam
            ));
        }
        let minimum = chrono::NaiveDate::MIN
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        for text in [
            "yesterday",
            "on:yesterday",
            "since:yesterday",
            "before:yesterday",
        ] {
            assert!(Search::parse(text, &minimum).is_err(), "{text}");
        }
        assert!(!search.matches("failed", u64::MAX, &Amsterdam));
        let fall = Amsterdam.with_ymd_and_hms(2026, 10, 25, 2, 30, 0);
        let search = Search::parse("2026-10-25", &now).unwrap();
        for at in [fall.earliest().unwrap(), fall.latest().unwrap()] {
            assert!(search.matches("", at.timestamp() as u64, &Amsterdam));
        }
    }
}
