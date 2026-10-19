//! Validated place identities shared by the catalog and privileged authority.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

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
    pub fn home() -> Self {
        Self {
            name: "Amsterdam".into(),
            country: "Netherlands".into(),
            region: "North Holland".into(),
            timezone: "Europe/Amsterdam".into(),
            latitude: 52.3676,
            longitude: 4.9041,
        }
    }
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
    pub fn same_place(&self, other: &Self) -> bool {
        self.name.eq_ignore_ascii_case(&other.name)
            && same_zone(&self.timezone, &other.timezone)
            && (self.latitude - other.latitude).abs() < 0.05
            && (self.longitude - other.longitude).abs() < 0.05
    }
}

pub(crate) fn canonical_with_table(name: &str, table: &str) -> Option<String> {
    name.parse::<chrono_tz::Tz>().ok()?;
    let links: HashMap<_, _> = table
        .lines()
        .filter_map(|line| {
            let words: Vec<_> = line.split_whitespace().collect();
            (words.len() >= 3 && matches!(words[0], "L" | "Link")).then(|| (words[2], words[1]))
        })
        .collect();
    let mut zone = name;
    for _ in 0..20 {
        match links.get(zone) {
            Some(target) => zone = target,
            None => return Some(zone.into()),
        }
    }
    None
}
pub(crate) fn same_zone(left: &str, right: &str) -> bool {
    static TABLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let directory = std::env::var_os("TZDIR").map(std::path::PathBuf::from);
        directory
            .iter()
            .map(|p| p.join("tzdata.zi"))
            .chain([
                Path::new("/etc/zoneinfo/tzdata.zi").to_owned(),
                Path::new("/usr/share/zoneinfo/tzdata.zi").to_owned(),
            ])
            .find_map(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    });
    matches!((canonical_with_table(left, table), canonical_with_table(right, table)), (Some(a),Some(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_are_not_equal_offsets() {
        let table = "L Europe/Brussels Europe/Amsterdam\n";
        assert_eq!(
            canonical_with_table("Europe/Amsterdam", table).as_deref(),
            Some("Europe/Brussels")
        );
        assert_ne!(
            canonical_with_table("Europe/Paris", table),
            canonical_with_table("Europe/Amsterdam", table)
        );
        assert_eq!(canonical_with_table("Invalid/Zone", table), None);
    }
    #[test]
    fn invalid_places_rejected() {
        let mut place = Place::home();
        place.latitude = f64::NAN;
        assert!(place.validate().is_err());
        place = Place::home();
        place.name = "\n".into();
        assert!(place.validate().is_err());
    }
}
