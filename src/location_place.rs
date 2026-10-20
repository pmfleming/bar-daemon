//! Validated place identities shared by the catalog and privileged authority.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Place {
    pub name: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub region: String,
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
}
impl Place {
    pub fn validate(&self) -> Result<()> {
        for text in [&self.name, &self.country, &self.region] {
            ensure!(
                text.len() <= 240 && !text.chars().any(char::is_control),
                "invalid place label"
            );
        }
        ensure!(!self.name.trim().is_empty(), "place name is required");
        ensure!(
            (-90.0..=90.0).contains(&self.latitude) && (-180.0..=180.0).contains(&self.longitude),
            "invalid place coordinates"
        );
        self.timezone
            .parse::<chrono_tz::Tz>()
            .context("invalid place timezone")?;
        Ok(())
    }
}

/// Parsed once per tzdata source; both authorities resolve the same bounded chains.
pub(crate) struct TimezoneLinks(HashMap<String, String>);
impl TimezoneLinks {
    pub fn from_table(table: &str) -> Self {
        Self(
            table
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    match (fields.next(), fields.next(), fields.next()) {
                        (Some("L" | "Link"), Some(target), Some(alias)) => {
                            Some((alias.into(), target.into()))
                        }
                        _ => None,
                    }
                })
                .collect(),
        )
    }

    pub fn resolve<'a>(&'a self, mut name: &'a str) -> Option<&'a str> {
        for _ in 0..20 {
            match self.0.get(name) {
                Some(target) => name = target,
                None => return Some(name),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_are_not_equal_offsets() {
        let links = TimezoneLinks::from_table("L Europe/Brussels Europe/Amsterdam\n");
        assert_eq!(links.resolve("Europe/Amsterdam"), Some("Europe/Brussels"));
        assert_ne!(
            links.resolve("Europe/Paris"),
            links.resolve("Europe/Amsterdam")
        );
        for (table, expected) in [
            ("Link B A\nL Europe/Dublin B\n", Some("Europe/Dublin")),
            ("L B A\nL A B\n", None),
            ("L A A\n", None),
            ("# ignored\nL incomplete\nZone A B\n", Some("A")),
            (
                "L Europe/Paris A\nL Europe/Dublin A\n",
                Some("Europe/Dublin"),
            ),
        ] {
            assert_eq!(TimezoneLinks::from_table(table).resolve("A"), expected);
        }
    }
    #[test]
    fn invalid_places_rejected() {
        let mut place = Place {
            name: "Amsterdam".into(),
            country: String::new(),
            region: String::new(),
            timezone: "Europe/Amsterdam".into(),
            latitude: f64::NAN,
            longitude: 4.9,
        };
        assert!(place.validate().is_err());
        place.latitude = 52.4;
        place.name = "\n".into();
        assert!(place.validate().is_err());
    }
}
