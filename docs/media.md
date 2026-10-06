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
  "content_type_source": "url"
}
```

- `source` is null when no supported, syntactically valid URL is supplied.
  Otherwise it contains the original URL and a nullable `service` key:
  `youtube`, `vimeo`, `soundcloud`, `spotify`, `pocketcasts`, or `audible`.
- `content_type_source` is `mpris`, `url`, or `unknown`. Recognized explicit
  `xesam:contentType` wins over URL inference. `audiobook` joins the existing
  `unknown`, `music`, `podcast` and `video` content kinds.
- Older snapshots deserialize with null source and unknown provenance. Clients
  must tolerate absent fields and unfamiliar service/content values.

A source is a **player-supplied claim**, not verified origin or authorization.
Never replace the MPRIS ID, root identity, desktop entry or capabilities with a
service label. All pinning, selection, seek and transport commands still use
that original player ID. Title, artist, album, artwork, rate and timing remain
unchanged; missing artwork/album and Zen's reported zero rate are not fabricated.

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

## Deferred enrichment

Provider API/oEmbed adapters and thumbnail retrieval are **not implemented**.
Future network enrichment should be explicitly configurable, use bounded fixed
provider endpoints, handle redirects/private-network access safely, and never
reuse browser cookies. Publish MPRIS data immediately; apply late enrichment only
to the same player owner and content generation, filling missing fields with
provenance rather than replacing reported metadata. Generic page scraping and
lookup failure must not block controls.

## Tests

`media::source::tests` covers service/path recognition, spoofed hosts, URL bounds,
unsafe schemes/credentials, explicit-metadata precedence and additive wire fields.
The isolated peer-D-Bus media test changes URLs under the same Firefox track ID,
checks hint clearing, and verifies supplied metadata, timing and capabilities
remain unchanged. No test touches the user's browser or playback.
