//! Offline hints from the player-supplied content URL. No requests, cookies,
//! artwork synthesis, or playback effects. Recomputed for every MPRIS snapshot.

use reqwest::Url;

use crate::model::{
    MediaContentType as Kind, MediaContentTypeSource as Origin, MediaSource,
    MediaSourceService as Service,
};

pub(super) struct SourceMetadata {
    pub source: Option<MediaSource>,
    pub content_type: Kind,
    pub content_type_source: Origin,
}

pub(super) fn resolve(raw_url: Option<&str>, content_type: Option<&str>) -> SourceMetadata {
    let resolved = raw_url.and_then(parse_source);
    let inferred = resolved.as_ref().map_or(Kind::Unknown, |(_, kind)| *kind);
    let explicit = match content_type
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "music" => Kind::Music,
        "podcast" => Kind::Podcast,
        "audiobook" => Kind::Audiobook,
        value if value == "video" || value.starts_with("video/") => Kind::Video,
        // Audio MIME types do not distinguish music from speech.
        _ => Kind::Unknown,
    };
    let (content_type, content_type_source) = if explicit != Kind::Unknown {
        (explicit, Origin::Mpris)
    } else if inferred != Kind::Unknown {
        (inferred, Origin::Url)
    } else {
        (Kind::Unknown, Origin::Unknown)
    };
    SourceMetadata {
        source: resolved.map(|(source, _)| source),
        content_type,
        content_type_source,
    }
}

fn parse_source(raw: &str) -> Option<(MediaSource, Kind)> {
    // Do not publish credentials or silently repair malformed player input.
    // Query/fragment values can also be sensitive: retain them only in transient
    // state, never log, persist, open, or fetch this URL.
    if raw.is_empty()
        || raw.len() > 8192
        || raw
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let (service, kind) = match url.scheme() {
        "http" | "https" => {
            let (scheme, rest) = raw.split_once("://")?;
            let authority = rest.split(['/', '?', '#']).next()?;
            if !scheme.eq_ignore_ascii_case(url.scheme())
                || authority.is_empty()
                || authority.contains('@')
                || url.host_str().is_none()
            {
                return None;
            }
            // Nonstandard ports remain usable metadata, but do not establish a
            // known service identity. Match exact parsed hosts, not substrings.
            if url.port().is_some() {
                (None, Kind::Unknown)
            } else {
                web_hints(&url)
            }
        }
        "spotify" if url.query().is_none() && url.fragment().is_none() => {
            let (category, id) = url.path().split_once(':')?;
            if !ascii_id(id) || !matches!(category, "track" | "episode") {
                return None;
            }
            (Some(Service::Spotify), spotify_kind(category))
        }
        // No local paths, browser-internal URLs, or arbitrary executable schemes.
        _ => return None,
    };
    Some((
        MediaSource {
            url: raw.to_owned(),
            service,
        },
        kind,
    ))
}

fn web_hints(url: &Url) -> (Option<Service>, Kind) {
    let host = url.host_str().unwrap_or("");
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let segments: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    match host {
        "youtube.com"
        | "www.youtube.com"
        | "m.youtube.com"
        | "music.youtube.com"
        | "youtu.be"
        | "www.youtube-nocookie.com"
        | "youtube-nocookie.com" => {
            let video = youtube_video(url).is_some();
            // Even music.youtube.com can play videos/podcasts; never infer Music.
            (
                Some(Service::Youtube),
                if video { Kind::Video } else { Kind::Unknown },
            )
        }
        "vimeo.com" | "www.vimeo.com" | "player.vimeo.com" => {
            let id = match (host, segments.as_slice()) {
                ("player.vimeo.com", ["video", id]) => Some(*id),
                ("vimeo.com" | "www.vimeo.com", [id]) => Some(*id),
                _ => None,
            };
            let video =
                id.is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
            (
                Some(Service::Vimeo),
                if video { Kind::Video } else { Kind::Unknown },
            )
        }
        "soundcloud.com" | "www.soundcloud.com" | "m.soundcloud.com" => {
            (Some(Service::Soundcloud), Kind::Unknown)
        }
        "open.spotify.com" => {
            let kind = match segments.as_slice() {
                [category, id] if ascii_id(id) => spotify_kind(category),
                _ => Kind::Unknown,
            };
            (Some(Service::Spotify), kind)
        }
        "pocketcasts.com" | "www.pocketcasts.com" | "play.pocketcasts.com" | "pca.st" => {
            (Some(Service::Pocketcasts), Kind::Unknown)
        }
        _ if audible_host(host) => (Some(Service::Audible), Kind::Unknown),
        _ => (None, Kind::Unknown),
    }
}

fn audible_host(host: &str) -> bool {
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("listen."))
        .unwrap_or(host);
    matches!(
        host,
        "audible.com"
            | "audible.co.uk"
            | "audible.de"
            | "audible.fr"
            | "audible.it"
            | "audible.es"
            | "audible.ca"
            | "audible.com.au"
            | "audible.co.jp"
            | "audible.in"
    )
}

fn ascii_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

// Online enrichment receives only this validated ID, never the original URL's
// query/fragment, credentials, arbitrary host or port.
pub(super) fn youtube_video_id(raw: &str) -> Option<String> {
    let (source, kind) = parse_source(raw)?;
    if source.service != Some(Service::Youtube) || kind != Kind::Video {
        return None;
    }
    youtube_video(&Url::parse(raw).ok()?)
}

fn youtube_video(url: &Url) -> Option<String> {
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let segments: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let id = if url.host_str() == Some("youtu.be") {
        match segments.as_slice() {
            [id] => (*id).to_owned(),
            _ => return None,
        }
    } else if path == "/watch" {
        let ids: Vec<_> = url.query_pairs().filter(|(key, _)| key == "v").collect();
        if ids.len() != 1 {
            return None;
        }
        ids[0].1.to_string()
    } else {
        match segments.as_slice() {
            ["shorts" | "embed" | "live", id] => (*id).to_owned(),
            _ => return None,
        }
    };
    youtube_id(&id).then_some(id)
}

fn youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn spotify_kind(category: &str) -> Kind {
    match category {
        "track" => Kind::Music,
        "episode" => Kind::Podcast,
        _ => Kind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_fields_are_additive_and_have_explicit_provenance() {
        use crate::model::MediaPlayer;
        let mut legacy = serde_json::to_value(MediaPlayer::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("source");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("content_type_source");
        let player: MediaPlayer = serde_json::from_value(legacy).unwrap();
        assert!(player.source.is_none());
        assert_eq!(player.content_type_source, Origin::Unknown);
        let resolved = resolve(Some("https://vimeo.com/123"), None);
        let value = serde_json::to_value(MediaPlayer {
            source: resolved.source,
            content_type: resolved.content_type,
            content_type_source: resolved.content_type_source,
            ..player
        })
        .unwrap();
        assert_eq!(value["source"]["url"], "https://vimeo.com/123");
        assert_eq!(value["source"]["service"], "vimeo");
        assert_eq!(value["content_type"], "video");
        assert_eq!(value["content_type_source"], "url");
    }

    #[test]
    fn exact_services_and_structural_content_hints() {
        for (url, service, kind) in [
            (
                "https://www.youtube.com/watch?v=RQzh-xnLRlM",
                Service::Youtube,
                Kind::Video,
            ),
            (
                "https://youtu.be/RQzh-xnLRlM?t=12",
                Service::Youtube,
                Kind::Video,
            ),
            (
                "https://www.youtube.com/shorts/RQzh-xnLRlM",
                Service::Youtube,
                Kind::Video,
            ),
            (
                "https://www.youtube-nocookie.com/embed/RQzh-xnLRlM",
                Service::Youtube,
                Kind::Video,
            ),
            (
                "https://music.youtube.com/watch?v=RQzh-xnLRlM",
                Service::Youtube,
                Kind::Video,
            ),
            (
                "https://www.youtube.com/@channel",
                Service::Youtube,
                Kind::Unknown,
            ),
            (
                "https://www.youtube.com/watch?v=RQzh-xnLRlM&v=other",
                Service::Youtube,
                Kind::Unknown,
            ),
            ("https://vimeo.com/123456", Service::Vimeo, Kind::Video),
            (
                "https://player.vimeo.com/video/123456",
                Service::Vimeo,
                Kind::Video,
            ),
            ("https://vimeo.com/channels", Service::Vimeo, Kind::Unknown),
            (
                "https://soundcloud.com/artist/recording",
                Service::Soundcloud,
                Kind::Unknown,
            ),
            (
                "https://open.spotify.com/episode/abc123?si=example",
                Service::Spotify,
                Kind::Podcast,
            ),
            (
                "https://open.spotify.com/track/abc123",
                Service::Spotify,
                Kind::Music,
            ),
            ("spotify:track:abc123", Service::Spotify, Kind::Music),
            ("spotify:episode:abc123", Service::Spotify, Kind::Podcast),
            (
                "https://open.spotify.com/playlist/abc123",
                Service::Spotify,
                Kind::Unknown,
            ),
            (
                "https://play.pocketcasts.com/podcasts/example",
                Service::Pocketcasts,
                Kind::Unknown,
            ),
            (
                "https://pca.st/example",
                Service::Pocketcasts,
                Kind::Unknown,
            ),
            (
                "https://www.audible.co.uk/pd/example",
                Service::Audible,
                Kind::Unknown,
            ),
            (
                "https://listen.audible.com/webplayer",
                Service::Audible,
                Kind::Unknown,
            ),
        ] {
            let resolved = resolve(Some(url), None);
            let source = resolved.source.unwrap();
            assert_eq!(source.url, url);
            assert_eq!(source.service, Some(service), "{url}");
            assert_eq!(resolved.content_type, kind, "{url}");
            assert_eq!(
                resolved.content_type_source,
                if kind == Kind::Unknown {
                    Origin::Unknown
                } else {
                    Origin::Url
                }
            );
        }
    }

    #[test]
    fn explicit_metadata_wins_without_guessing_from_audio_mime() {
        for (metadata, expected) in [
            ("podcast", Kind::Podcast),
            ("music", Kind::Music),
            ("audiobook", Kind::Audiobook),
            ("VIDEO/MP4", Kind::Video),
        ] {
            let resolved = resolve(Some("https://open.spotify.com/track/123"), Some(metadata));
            assert_eq!(resolved.content_type, expected);
            assert_eq!(resolved.content_type_source, Origin::Mpris);
        }
        let resolved = resolve(None, Some("audio/mpeg"));
        assert_eq!(resolved.content_type, Kind::Unknown);
        assert_eq!(resolved.content_type_source, Origin::Unknown);
        assert!(resolved.source.is_none());
    }

    #[test]
    fn unknown_urls_are_preserved_but_never_guessed_or_fetched() {
        for url in [
            "https://example.org/episode.mp3?title=YouTube#chapter",
            "http://localhost:8080/video",
            "https://192.168.1.1/track",
            "https://youtube.com.evil.test/watch?v=RQzh-xnLRlM",
            "https://notyoutube.com/watch?v=RQzh-xnLRlM",
            "https://example.org/youtube.com/watch?v=RQzh-xnLRlM",
            "https://www.audible.com.evil.test/book",
            "https://soundcloud.com:8443/recording",
        ] {
            let resolved = resolve(Some(url), None);
            assert_eq!(
                resolved.source,
                Some(MediaSource {
                    url: url.into(),
                    service: None
                })
            );
            assert_eq!(resolved.content_type, Kind::Unknown);
        }
    }

    #[test]
    fn unsafe_or_malformed_sources_do_not_escape_into_state() {
        for url in [
            "",
            "not a url",
            "/home/user/song.mp3",
            "file:///home/user/song.mp3",
            "javascript:alert(1)",
            "data:text/plain,hello",
            "about:blank",
            "https://user:secret@youtube.com/watch?v=RQzh-xnLRlM",
            "https://youtube.com@evil.test/watch?v=RQzh-xnLRlM",
            "https://youtube.com/\nwatch?v=RQzh-xnLRlM",
            "https://youtube.com\\@evil.test/",
            "https:youtube.com/watch?v=RQzh-xnLRlM",
            "https:///youtube.com/watch?v=RQzh-xnLRlM",
            "https:youtube.com/watch?redirect=https://example.org",
            "https://@youtube.com/watch?v=RQzh-xnLRlM",
            "spotify:track:",
            "spotify:track:123/extra",
            "spotify:track:123?secret=value",
        ] {
            assert!(resolve(Some(url), None).source.is_none(), "{url}");
        }
        assert!(
            resolve(
                Some(&format!("https://example.org/{}", "a".repeat(8192))),
                None
            )
            .source
            .is_none()
        );
    }
}
