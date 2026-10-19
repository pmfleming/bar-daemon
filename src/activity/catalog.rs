//! Durable saved cities. Declarative sources are imported once, never replayed on refresh.
use super::{
    config::{ActivityConfig, WeatherConfig},
    locations::Location,
    model::{WeatherState, WorldClockState},
};
use crate::{location_place::Place, paths::save_json_atomic};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

pub(crate) const HOME: &str = "weather:home";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct City {
    pub id: String,
    pub place: Place,
    pub has_coordinates: bool,
    pub weather: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Catalog {
    pub version: u32,
    pub revision: u64,
    pub next_id: u64,
    pub cities: Vec<City>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CatalogInfo {
    pub revision: u64,
    pub writable: bool,
}
impl Catalog {
    pub(crate) fn protected_home() -> Self {
        Self::seed(&ActivityConfig::default())
    }
    fn seed(config: &ActivityConfig) -> Self {
        let mut cities = vec![City {
            id: HOME.into(),
            place: Place::home(),
            has_coordinates: true,
            weather: true,
        }];
        for weather in config.configured_weather_locations().iter() {
            let timezone = if weather.timezone == "auto" {
                tzf_rs::DefaultFinder::new()
                    .get_tz_name(weather.longitude, weather.latitude)
                    .to_owned()
            } else {
                weather.timezone.clone()
            };
            let place = Place {
                name: weather.location.clone(),
                country: String::new(),
                region: String::new(),
                timezone,
                latitude: weather.latitude,
                longitude: weather.longitude,
            };
            if place.same_place(&Place::home()) {
                continue;
            }
            let id = if weather.id == "home" {
                "weather:imported-home".into()
            } else {
                format!("weather:{}", weather.id)
            };
            cities.push(City {
                id,
                place,
                has_coordinates: true,
                weather: true,
            });
        }
        let mut clocks = config.world_clocks.clone();
        clocks.sort_by(|a, b| (&a.timezone, &a.label).cmp(&(&b.timezone, &b.label)));
        let mut zones: HashSet<String> = cities.iter().map(|c| c.place.timezone.clone()).collect();
        for clock in clocks {
            if !zones.insert(clock.timezone.clone()) {
                continue;
            }
            let name = if clock.label.is_empty() {
                clock
                    .timezone
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .replace('_', " ")
            } else {
                clock.label
            };
            cities.push(City {
                id: format!("clock:{}", clock.timezone),
                place: Place {
                    name,
                    country: String::new(),
                    region: String::new(),
                    timezone: clock.timezone,
                    latitude: 0.0,
                    longitude: 0.0,
                },
                has_coordinates: false,
                weather: false,
            });
        }
        Self {
            version: 1,
            revision: 1,
            next_id: 1,
            cities,
        }
    }
    pub async fn load_or_import(path: &Path, config: &ActivityConfig) -> Result<Self> {
        // Bounded/no-symlink native reader; missing is the only import condition.
        let bytes = shelllist_daemon_core::read_bytes_bounded(path, 256 * 1024)?;
        let value = match bytes {
            Some(bytes) => serde_json::from_slice(&bytes).context("invalid city store")?,
            None => Self::seed(config),
        };
        value.validate()?;
        if !path.exists() {
            save_json_atomic(path, &value).await?;
        }
        Ok(value)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.revision > 0 && self.next_id > 0 && self.cities.len() <= 32,
            "unsupported or oversized city store"
        );
        let mut ids = HashSet::new();
        for city in &self.cities {
            city.place.validate()?;
            ensure!(
                city.id.len() <= 240 && city.id != "current-location" && ids.insert(&city.id),
                "invalid city identity"
            );
        }
        let home = self
            .cities
            .iter()
            .find(|c| c.id == HOME)
            .context("Home missing")?;
        ensure!(
            home.place == Place::home() && home.weather && home.has_coordinates,
            "Amsterdam Home is protected"
        );
        Ok(())
    }
    pub fn add(&self, place: Place, revision: u64) -> Result<(Self, String)> {
        ensure!(
            revision == self.revision,
            "City list changed; refresh before saving"
        );
        place.validate()?;
        if let Some(existing) = self
            .cities
            .iter()
            .find(|c| c.has_coordinates && c.place.same_place(&place))
        {
            return Ok((self.clone(), existing.id.clone()));
        }
        ensure!(
            self.cities.len() < 32,
            "At most 32 saved cities (including Home) are supported"
        );
        let mut next = self.clone();
        let id = format!("city:{}", next.next_id);
        next.next_id = next.next_id.checked_add(1).context("city ID exhausted")?;
        next.revision = next
            .revision
            .checked_add(1)
            .context("city revision exhausted")?;
        next.cities.push(City {
            id: id.clone(),
            place,
            has_coordinates: true,
            weather: true,
        });
        next.validate()?;
        Ok((next, id))
    }
    pub fn remove(&self, id: &str, revision: u64) -> Result<Self> {
        ensure!(
            id != HOME && id != "current-location",
            "Home and Current location cannot be removed"
        );
        ensure!(
            revision == self.revision,
            "City list changed; refresh before removing"
        );
        ensure!(
            self.cities.iter().any(|c| c.id == id),
            "City no longer exists"
        );
        let mut next = self.clone();
        next.cities.retain(|c| c.id != id);
        next.revision = next
            .revision
            .checked_add(1)
            .context("city revision exhausted")?;
        next.validate()?;
        Ok(next)
    }
    pub fn weather_configs(&self) -> Vec<WeatherConfig> {
        self.cities
            .iter()
            .filter(|c| c.weather)
            .map(|c| WeatherConfig {
                id: c.id.clone(),
                location: c.place.name.clone(),
                home: c.id == HOME,
                latitude: c.place.latitude,
                longitude: c.place.longitude,
                timezone: c.place.timezone.clone(),
            })
            .collect()
    }
    pub fn project(
        &self,
        forecasts: &[WeatherState],
        instant: chrono::DateTime<chrono::Utc>,
    ) -> Vec<Location> {
        self.cities
            .iter()
            .map(|city| {
                let mut clock =
                    WorldClockState::at(&city.place.timezone, &city.place.name, instant)
                        .unwrap_or_default();
                clock.city = city.place.name.clone();
                let weather = city.weather.then(|| {
                    forecasts
                        .iter()
                        .find(|w| w.id == city.id)
                        .cloned()
                        .unwrap_or_else(|| WeatherState {
                            id: city.id.clone(),
                            location: city.place.name.clone(),
                            home: city.id == HOME,
                            timezone: city.place.timezone.clone(),
                            latitude: city.place.latitude,
                            longitude: city.place.longitude,
                            ..Default::default()
                        })
                });
                Location {
                    id: city.id.clone(),
                    clock,
                    home: city.id == HOME,
                    has_coordinates: city.has_coordinates,
                    latitude: city.place.latitude,
                    longitude: city.place.longitude,
                    has_weather: city.weather,
                    weather,
                    place: city.has_coordinates.then(|| city.place.clone()),
                    removable: city.id != HOME,
                    ..Default::default()
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn durable_import_mutations_and_protected_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locations.json");
        let first = Catalog::load_or_import(&path, &ActivityConfig::default())
            .await
            .unwrap();
        assert_eq!(first.cities.len(), 1);
        assert!(first.remove(HOME, 1).is_err());
        let mut place = Place::home();
        place.name = "Haarlem".into();
        place.latitude = 52.387;
        place.longitude = 4.646;
        let (next, id) = first.add(place, 1).unwrap();
        assert_eq!(next.cities.len(), 2);
        save_json_atomic(&path, &next).await.unwrap();
        let loaded = Catalog::load_or_import(&path, &ActivityConfig::default())
            .await
            .unwrap();
        assert_eq!(loaded.revision, 2);
        assert!(loaded.remove(&id, 1).is_err());
        let removed = loaded.remove(&id, 2).unwrap();
        save_json_atomic(&path, &removed).await.unwrap();
        assert_eq!(
            Catalog::load_or_import(&path, &ActivityConfig::default())
                .await
                .unwrap()
                .cities
                .len(),
            1
        );
        std::fs::write(&path, b"broken").unwrap();
        assert!(
            Catalog::load_or_import(&path, &ActivityConfig::default())
                .await
                .is_err()
        );
    }
    #[test]
    fn at_home_projection_retains_home_coordinates_and_saved_cities() {
        let catalog = Catalog::seed(&ActivityConfig::default());
        let instant = chrono::Utc::now();
        let mut rows = catalog.project(&[], instant);
        let local = crate::model::TimezoneState {
            accepted_location: Some(crate::model::AcceptedLocation {
                timezone: "Europe/Amsterdam".into(),
                latitude: 52.38,
                longitude: 4.64,
                verified_at: 1.0,
                source: crate::model::LocationSource::Ip,
                accuracy_meters: None,
            }),
            ..Default::default()
        };
        super::super::locations::append_current(&mut rows, &local, instant);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].home && rows[0].current);
        assert_eq!(rows[0].latitude, Place::home().latitude);
        let mut away = local;
        away.accepted_location.as_mut().unwrap().timezone = "Europe/Paris".into();
        let mut rows = catalog.project(&[], instant);
        super::super::locations::append_current(&mut rows, &away, instant);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].id, "current-location");
        let mut rows = catalog.project(&[], instant);
        super::super::locations::append_current(&mut rows, &Default::default(), instant);
        assert!(rows[1].clock_unavailable);
    }
    #[test]
    fn duplicates_and_distinct_same_zone_places() {
        let catalog = Catalog::seed(&ActivityConfig::default());
        let (same, id) = catalog.add(Place::home(), 1).unwrap();
        assert_eq!(id, HOME);
        assert_eq!(same.revision, 1);
        let mut other = Place::home();
        other.name = "Haarlem".into();
        let (next, _) = catalog.add(other, 1).unwrap();
        assert_eq!(next.cities.len(), 2);
    }
}
