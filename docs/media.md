# Media source metadata

MPRIS remains the owner of player identity, metadata and playback capabilities.
The daemon's first-pass source resolver adds **offline presentation hints** from
`xesam:url`; it performs no network requests, browser inspection, cookie access,
artwork synthesis or playback operations.

## Additive bar-api v1 fields

Each player now carries:

```json
{
  "id": "org.mpris.MediaPlayer2.firefox.instance_1_380",
  "identity": "Mozilla zen",
  "desktop_entry": "zen",
  "source": {
    "url": "https://www.youtube.com/watch?v=RQzh-xnLRlM",
    "service": "youtube"
  },
  "content_type": "video",
  "content_type_source": "url",
  "metadata_sources": {}
}
```

- `source` is null when no supported, syntactically valid URL is supplied.
  Otherwise it contains the original URL and a nullable `service` key:
  `youtube`, `vimeo`, `soundcloud`, `spotify`, `pocketcasts`, or `audible`.
- `content_type_source` is `mpris`, `url`, or `unknown`. Recognized explicit
  `xesam:contentType` wins over URL inference. `audiobook` joins the existing
  `unknown`, `music`, `podcast` and `video` content kinds.
- `metadata_sources` defaults to `{}` for older/unenriched snapshots. Only fields
  filled by the online fallback (`title`, `artist`, `art_url`) are marked
  `youtube-oembed`; absence means the original player value. Internal unique-owner
  and content-generation tracking never enters the wire model.
- Older snapshots deserialize with null source and unknown provenance. Clients
  must tolerate absent fields and unfamiliar service/content values.

A source is a **player-supplied claim**, not verified origin or authorization.
Never replace the MPRIS ID, root identity, desktop entry or capabilities with a
service label. All pinning, selection, seek and transport commands still use
that original player ID. Supplied title, artist, album, artwork, rate and timing
remain unchanged. The optional online fallback below fills only missing title,
artist/channel and artwork; it never fabricates an album, rate or duration.

## Recognition policy

Only exact parsed hostnames and deliberately supported aliases are recognized;
no title, artwork URL, substring or arbitrary subdomain guessing occurs.

| Service | Offline content inference |
| --- | --- |
| YouTube | Valid video IDs in watch, short-link, shorts, embed and live paths → video; Music host does not imply music |
| Vimeo | Numeric video paths on the main site and player embed → video |
| Spotify | Track/episode paths and native `spotify:track:` / `spotify:episode:` URIs → music/podcast |
| SoundCloud | Service only; recordings may be music or speech |
| Pocket Casts | Service only; a site/player URL is not an episode identifier |
| Audible | Service only; includes an explicit regional-domain allowlist |
| Other HTTP(S) sources | URL retained, service and inferred kind unknown |

Home, channel, playlist and unrecognized paths may establish service identity
without establishing a content kind. Audio MIME types and URL file extensions do
not distinguish speech from music. The resolver does not follow short-link
redirects or discover endpoints. Unsupported paths/aliases remain conservative
fallbacks, not failures that hide the player.

URLs are bounded to 8 KiB. Credential-bearing URLs, whitespace/control characters,
backslashes, malformed authorities, local paths, file URLs and other schemes are
not exported as sources. Nonstandard ports do not establish a known service.
Unknown/private HTTP(S) addresses can be retained as metadata but are **never
fetched**. Query and fragment values are preserved, not stripped heuristically:
they can identify content, but may also contain sensitive tokens. Source URLs
are transient snapshot/event data, not logged or persisted by the resolver;
consumers should treat them as potentially sensitive and not auto-open/fetch them.

Hints are recomputed on each metadata read, so a cleared/changed URL clears or
replaces the previous service. There is no cache keyed on `mpris:trackid`: Firefox
and Zen can reuse `/org/mpris/MediaPlayer2/firefox` across different content.

## Optional YouTube enrichment

**Off by default.** Set `BAR_DAEMON_YOUTUBE_METADATA=1` (or `true`) in the daemon's
service environment and restart it to enable. Shelllist's NixOS/Home Manager
modules expose `programs.shelllist.media.youtubeMetadata.enable = true` for the
managed daemon. This is an explicit privacy choice: video IDs and the client's IP
are disclosed to YouTube even for paused sessions. No API key or browser extension
is required. Cookies, authorization headers, browser profiles and history are not
read. Original source query/fragment parameters are never forwarded.

Only structurally recognized watch/short-link/shorts/embed/live URLs qualify.
The daemon requests `https://www.youtube.com/oembed` with a canonical watch URL
built from the validated 11-character ID. It fills missing title and artist from
`title` and `author_name`, ignoring HTML/author links and unexpected response types.
Supplied player values always win. Publicly unavailable/private/restricted videos
can fail and retain their existing metadata and placeholder; playback never waits.

Returned thumbnails must use HTTPS on `i.ytimg.com`, the same video ID and an
allowlisted `/vi/…/*.jpg` or `/vi_webp/…/*.webp` thumbnail path. They are downloaded
by the daemon, not handed to QML as remote URLs. Both requests reject redirects,
ambient proxies, non-public DNS results (including mapped IPv4) and unexpected
content types. Connection/whole-request timeouts are 2/4 seconds per request.
JSON is capped at 64 KiB; images at 1 MiB, with JPEG/PNG/WebP signature checks.
Text is bounded to 4096 bytes per field; control-bearing values are discarded.
A thumbnail failure does not discard useful title/channel data.

At most four lookups run concurrently and four start per ten-second window.
Excess work waits for a later observation; no polling loop is added. A 32-entry
LRU cache is keyed by video ID: six hours for complete artwork results, five
minutes for failures or partial results, checked on subsequent observations.
No persistent metadata database is created. Artwork lives in randomly named,
owner-only temporary files; current sessions retain their files through cache
eviction, and normal session/cache teardown unlinks them. As with other temporary
files, abrupt process termination can leave files for system temp cleanup.
Source URLs and HTTP errors containing IDs are not logged by enrichment.

MPRIS data is published immediately. Completion rereads current MPRIS metadata
and checks a unique D-Bus owner plus an observed content generation (URL and raw
metadata), including A→B→A changes and Firefox's reused track ID. Removed or
changed players never receive the old ticket's result. Cached public metadata can
be reused on a fresh observation for the same video ID, without reusing a player's
selection, controls or timing. Later supplied metadata supersedes every fallback.
The existing selection/pinning lock and capability guards remain authoritative.

## Tests

`media::source::tests` covers service/path recognition, spoofed hosts, URL bounds,
unsafe schemes/credentials, explicit-metadata precedence and additive wire fields.
The isolated peer-D-Bus media test changes URLs under the same Firefox track ID,
checks hint clearing, and verifies supplied metadata, timing and capabilities
remain unchanged. Enrichment tests cover default-off behavior, missing-field
precedence/provenance, owner/generation/removal guards, negative caching, request
budgets, eviction and temporary-file lifetime. Loopback HTTP tests cover status,
redirect, content-type, body-limit and timeout failures; pure tests cover canonical
URLs, thumbnail paths, private DNS ranges and malformed payloads. No test contacts
YouTube or touches the user's browser or playback.
