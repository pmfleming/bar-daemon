//! Offline hints from the player-supplied content URL. No requests, cookies,
//! artwork synthesis, or playback effects. Recomputed for every MPRIS snapshot.

use std::borrow::Cow;

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
    let resolved = raw_url.and_then(media_url).map(|url| {
        if url.scheme() == "spotify" {
            (
                Some(Service::Spotify),
                spotify_kind(url.path().split(':').next().unwrap_or_default()),
            )
        } else {
            web_hints(&url)
        }
    });
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
        source: resolved
            .zip(raw_url)
            .map(|((service, _), raw)| MediaSource {
                url: raw.to_owned(),
                service,
            }),
        content_type,
        content_type_source,
    }
}

// Shared lexical boundary; each caller still restricts schemes/hosts/paths.
// Never silently repair whitespace/backslashes or retain URL credentials.
pub(super) fn parse_url(raw: &str) -> Option<Url> {
    if raw
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    (url.username().is_empty() && url.password().is_none()).then_some(url)
}

fn media_url(raw: &str) -> Option<Url> {
    // Query/fragment values remain transient: never log, persist, open or fetch.
    if raw.len() > 8192 {
        return None;
    }
    let url = parse_url(raw)?;
    match url.scheme() {
        "http" | "https" => {
            // Require the literal scheme/authority, not a :// inside the query
            // of a repaired URL such as https:host?redirect=https://other.
            let (scheme, rest) = raw.split_once("://")?;
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            if !scheme.eq_ignore_ascii_case(url.scheme())
                || authority.is_empty()
                || authority.contains('@')
            {
                return None;
            }
        }
        "spotify" if url.query().is_none() && url.fragment().is_none() => {
            let (category, id) = url.path().split_once(':')?;
            if !ascii_id(id) || !matches!(category, "track" | "episode") {
                return None;
            }
        }
        // No local paths, browser-internal URLs, or arbitrary executable schemes.
        _ => return None,
    }
    Some(url)
}

fn web_service(url: &Url) -> Option<Service> {
    // Nonstandard ports remain usable metadata but establish no service identity.
    if url.port().is_some() {
        return None;
    }
    Some(match url.host_str()? {
        "youtube.com"
        | "www.youtube.com"
        | "m.youtube.com"
        | "music.youtube.com"
        | "youtu.be"
        | "www.youtube-nocookie.com"
        | "youtube-nocookie.com" => Service::Youtube,
        "vimeo.com" | "www.vimeo.com" | "player.vimeo.com" => Service::Vimeo,
        "soundcloud.com" | "www.soundcloud.com" | "m.soundcloud.com" => Service::Soundcloud,
        "open.spotify.com" => Service::Spotify,
        "pocketcasts.com" | "www.pocketcasts.com" | "play.pocketcasts.com" | "pca.st" => {
            Service::Pocketcasts
        }
        host if audible_host(host) => Service::Audible,
        _ => return None,
    })
}

// Three slots distinguish one/two-component paths from paths with extra segments
// without allocating a Vec. Strip only one trailing slash, as before.
fn path_parts(url: &Url) -> [Option<&str>; 3] {
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let mut parts = path.strip_prefix('/').unwrap_or(path).split('/');
    std::array::from_fn(|_| parts.next())
}

fn web_hints(url: &Url) -> (Option<Service>, Kind) {
    let service = web_service(url);
    let kind = match (service, path_parts(url)) {
        // Even music.youtube.com can play videos/podcasts; never infer Music.
        (Some(Service::Youtube), _) if youtube_video(url).is_some() => Kind::Video,
        (Some(Service::Vimeo), parts) => {
            let id = match (url.host_str(), parts) {
                (Some("player.vimeo.com"), [Some("video"), Some(id), None]) => id,
                (Some("vimeo.com" | "www.vimeo.com"), [Some(id), None, None]) => id,
                _ => "",
            };
            if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
                Kind::Video
            } else {
                Kind::Unknown
            }
        }
        (Some(Service::Spotify), [Some(category), Some(id), None]) if ascii_id(id) => {
            spotify_kind(category)
        }
        _ => Kind::Unknown,
    };
    (service, kind)
}

fn audible_host(host: &str) -> bool {
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("listen."))
        .unwrap_or(host);
    audible_domain(host)
}

pub(super) fn audible_domain(host: &str) -> bool {
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
    let url = media_url(raw)?;
    if web_service(&url) != Some(Service::Youtube) {
        return None;
    }
    youtube_video(&url).map(Cow::into_owned)
}

fn youtube_video(url: &Url) -> Option<Cow<'_, str>> {
    let id = match (url.host_str(), path_parts(url)) {
        (Some("youtu.be"), [Some(id), None, None]) => Cow::Borrowed(id),
        (Some("youtu.be"), _) => return None,
        (_, [Some("watch"), None, None]) => {
            let mut ids = url.query_pairs().filter(|(key, _)| key == "v");
            let id = ids.next()?.1;
            if ids.next().is_some() {
                return None;
            }
            id
        }
        (_, [Some("shorts" | "embed" | "live"), Some(id), None]) => Cow::Borrowed(id),
        _ => return None,
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
    use super::{Kind, Origin, Service, resolve};
    use crate::model::MediaSource;

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
    fn path_arity_and_decoded_ids_keep_the_same_trust_boundary() {
        let id = "RQzh-xnLRlM";
        for (url, kind, video) in [
            (format!("https://youtu.be/{id}/"), Kind::Video, Some(id)),
            (format!("https://youtu.be/shorts/{id}"), Kind::Unknown, None),
            (
                "https://youtube.com/watch/?v=%52Qzh-xnLRlM".into(),
                Kind::Video,
                Some(id),
            ),
            (
                format!("https://youtube.com/watch//?v={id}"),
                Kind::Unknown,
                None,
            ),
            (
                format!("https://youtube.com/embed/{id}/extra"),
                Kind::Unknown,
                None,
            ),
            ("https://vimeo.com/123/".into(), Kind::Video, None),
            ("https://vimeo.com/123//".into(), Kind::Unknown, None),
            (
                "https://player.vimeo.com/video/123/extra".into(),
                Kind::Unknown,
                None,
            ),
            (
                "https://open.spotify.com/track/abc123/".into(),
                Kind::Music,
                None,
            ),
            (
                "https://open.spotify.com/track/abc123//".into(),
                Kind::Unknown,
                None,
            ),
        ] {
            assert_eq!(resolve(Some(&url), None).content_type, kind, "{url}");
            assert_eq!(super::youtube_video_id(&url).as_deref(), video, "{url}");
        }
        let url = super::media_url(&format!("https://youtu.be/{id}")).unwrap();
        assert!(matches!(
            super::youtube_video(&url),
            Some(std::borrow::Cow::Borrowed(_))
        ));
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
