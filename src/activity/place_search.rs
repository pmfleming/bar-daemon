//! Bounded read-only city search; no frontend HTTP or implicit city writes.
use crate::location_place::Place;
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::time::Duration;

pub(crate) async fn search(query: &str) -> Result<Vec<Place>> {
    let query = query.trim();
    ensure!(
        (2..=120).contains(&query.chars().count()) && !query.chars().any(char::is_control),
        "Enter 2–120 characters"
    );
    static GATE: tokio::sync::Mutex<Option<tokio::time::Instant>> =
        tokio::sync::Mutex::const_new(None);
    let mut gate = GATE
        .try_lock()
        .map_err(|_| anyhow::anyhow!("City search already in progress"))?;
    ensure!(
        gate.is_none_or(|last| last.elapsed() >= Duration::from_millis(500)),
        "City search rate limited; try again"
    );
    *gate = Some(tokio::time::Instant::now());
    let client = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut response = client
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[("name", query), ("count", "12"), ("format", "json")])
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 65536,
            "City search response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    #[derive(Deserialize)]
    struct Reply {
        #[serde(default)]
        results: Vec<Candidate>,
    }
    #[derive(Deserialize)]
    struct Candidate {
        name: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        admin1: String,
        timezone: Option<String>,
        latitude: f64,
        longitude: f64,
    }
    let reply: Reply = serde_json::from_slice(&bytes)?;
    Ok(reply
        .results
        .into_iter()
        .take(12)
        .filter_map(|r| {
            let place = Place {
                name: r.name,
                country: r.country,
                region: r.admin1,
                timezone: r.timezone?,
                latitude: r.latitude,
                longitude: r.longitude,
            };
            place.validate().ok().map(|_| place)
        })
        .collect())
}
