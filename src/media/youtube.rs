//! Opt-in, fixed-endpoint YouTube oEmbed. Never fetch the player-supplied URL,
//! HTML, embeds, cookies or redirects. Artwork is a bounded private temporary file.
use std::{io::Write, net::IpAddr, sync::Arc, time::Duration};

use anyhow::{Result, bail};
use reqwest::{
    Client, Url,
    dns::{Addrs, Name, Resolve, Resolving},
};
use serde::Deserialize;
use tempfile::NamedTempFile;

use crate::model::MediaPlayer;

const JSON_LIMIT: usize = 64 * 1024;
const IMAGE_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Default)]
pub(super) struct Metadata {
    pub title: String,
    pub artist: String,
    pub artwork: Option<Arc<NamedTempFile>>,
}

impl Metadata {
    pub fn apply(&self, player: &mut MediaPlayer) {
        for (field, target, value) in [
            ("title", &mut player.title, &self.title),
            ("artist", &mut player.artist, &self.artist),
        ] {
            if target.trim().is_empty() && !value.is_empty() {
                *target = value.clone();
                player
                    .metadata_sources
                    .insert(field.into(), "youtube-oembed".into());
            }
        }
        if player.art_url.trim().is_empty()
            && let Some(file) = &self.artwork
            && let Ok(url) = Url::from_file_path(file.path())
        {
            player.art_url = url.to_string();
            player
                .metadata_sources
                .insert("art_url".into(), "youtube-oembed".into());
        }
    }
}

#[derive(Clone)]
pub(super) struct Fetcher {
    client: Client,
}

impl Fetcher {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .https_only(true)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .dns_resolver(Arc::new(PublicResolver))
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(4))
                .build()?,
        })
    }

    pub async fn fetch(&self, id: &str) -> Result<Metadata> {
        let body = bounded_get(
            &self.client,
            oembed_url(id),
            JSON_LIMIT,
            &["application/json"],
        )
        .await?;
        let (mut metadata, thumbnail) = parse(&body, id)?;
        if let Some(url) = thumbnail {
            // A thumbnail failure must not discard useful title/channel data.
            metadata.artwork = download_artwork(&self.client, url).await.ok();
        }
        Ok(metadata)
    }
}

async fn download_artwork(client: &Client, url: Url) -> Result<Arc<NamedTempFile>> {
    let bytes = bounded_get(
        client,
        url,
        IMAGE_LIMIT,
        &["image/jpeg", "image/png", "image/webp"],
    )
    .await?;
    if !image_signature(&bytes) {
        bail!("invalid thumbnail signature");
    }
    tokio::task::spawn_blocking(move || -> Result<_> {
        let mut file = tempfile::Builder::new()
            .prefix(".bar-youtube-")
            .tempfile()?;
        file.write_all(&bytes)?;
        file.flush()?;
        Ok(Arc::new(file))
    })
    .await?
}

fn oembed_url(id: &str) -> Url {
    let mut url = Url::parse("https://www.youtube.com/oembed").expect("fixed endpoint");
    url.query_pairs_mut()
        .append_pair("url", &format!("https://www.youtube.com/watch?v={id}"))
        .append_pair("format", "json");
    url
}

#[derive(Deserialize)]
struct Oembed {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    author_name: String,
    #[serde(default)]
    thumbnail_url: String,
}

fn text(value: &str) -> String {
    let value = value.trim();
    if value.len() <= 4096 && !value.chars().any(char::is_control) {
        value.to_owned()
    } else {
        String::new()
    }
}

fn parse(body: &[u8], id: &str) -> Result<(Metadata, Option<Url>)> {
    let value: Oembed = serde_json::from_slice(body)?;
    if value.kind != "video" {
        bail!("unexpected oEmbed kind");
    }
    Ok((
        Metadata {
            title: text(&value.title),
            artist: text(&value.author_name),
            artwork: None,
        },
        thumbnail_url(&value.thumbnail_url, id),
    ))
}

fn thumbnail_url(raw: &str, id: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("i.ytimg.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || raw
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let parts: Vec<_> = url.path().split('/').collect();
    match parts.as_slice() {
        ["", "vi", video, file]
            if *video == id
                && matches!(
                    *file,
                    "default.jpg"
                        | "mqdefault.jpg"
                        | "hqdefault.jpg"
                        | "sddefault.jpg"
                        | "maxresdefault.jpg"
                ) =>
        {
            Some(url.clone())
        }
        ["", "vi_webp", video, file]
            if *video == id
                && matches!(
                    *file,
                    "default.webp"
                        | "mqdefault.webp"
                        | "hqdefault.webp"
                        | "sddefault.webp"
                        | "maxresdefault.webp"
                ) =>
        {
            Some(url.clone())
        }
        _ => None,
    }
}

fn image_signature(body: &[u8]) -> bool {
    body.starts_with(b"\xff\xd8\xff")
        || body.starts_with(b"\x89PNG\r\n\x1a\n")
        || (body.starts_with(b"RIFF") && body.get(8..12) == Some(b"WEBP"))
}

async fn bounded_get(client: &Client, url: Url, limit: usize, types: &[&str]) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?;
    if !response.status().is_success() {
        bail!("provider request failed");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if !types.contains(&content_type)
        || response
            .content_length()
            .is_some_and(|len| len > limit as u64)
    {
        bail!("invalid provider response");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > limit {
            bail!("provider response too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

// Validate and pin the actual DNS results used by the connection. Redirects and
// ambient proxies are disabled; even an allowlisted host cannot reach a LAN IP.
struct PublicResolver;
impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            if !matches!(name.as_str(), "www.youtube.com" | "i.ytimg.com") {
                return Err(std::io::Error::other("unexpected metadata host").into());
            }
            let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
                return Err(std::io::Error::other("non-public metadata address").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..128).contains(&b))
                && !(a == 198 && (18..20).contains(&b))
                && !(a == 192 && b == 0)
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(ip));
            }
            let parts = ip.segments();
            (parts[0] & 0xe000) == 0x2000
                && parts[0] != 0x2002
                && !(parts[0] == 0x2001 && (parts[1] < 0x200 || parts[1] == 0xdb8))
                && !(parts[0] == 0x3fff && parts[1] < 0x1000)
        }
    }
}

#[cfg(test)]
mod tests;
