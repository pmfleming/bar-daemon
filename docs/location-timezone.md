# GeoClue-first timezone authority

`bar-location-daemon` is the separate **system** timezone writer. The session
`bar-daemon` only observes timedated, public status and the authorized accepted
location reader. Shelllist panels and weather refreshes never set the timezone.
UTC/NTP, saved Home/weather locations and world clocks are not changed.

## Policy

1. Ask GeoClue for a usable, fresh location.
2. If GeoClue positively identifies an IP-only result or IP fallback, accept its
   coordinate-derived timezone without making an independent IP request.
3. For all other sources (database, GPS, other non-IP, or unknown provenance),
   query `https://ipapi.co/json/` and compare the two coordinate-derived timezones.
4. Matching **canonical IANA zones** permit automatic application. Different
   zones retain the current timezone and wait for a new connection or reload.

There is **no distance requirement**. Two places hundreds of kilometres apart
in one timezone agree; neighbouring places on opposite sides of a timezone border
do not. Equal current UTC offsets are not timezone identity (DST rules can differ).
Aliases such as `Eire` canonicalize before comparison. The IP provider's country
and timezone labels are not extra agreement vetoes: both coordinate pairs use the
same offline resolver. Reported GeoClue accuracy must be finite and nonnegative,
but the former 25 km Wi-Fi-only accuracy limit is gone, including for IP fallback.

Missing/invalid/expired evidence, unavailable services, ambiguous boundaries,
HTTP errors and rate limits are **pending confirmation**, not disagreement.
They retain the current timezone and retry. A failed GeoClue request does not
cause a direct-IP fallback: the accepted IP path is specifically GeoClue's result.
Manual overrides and uncertain writes remain independent safety fences.

## Provenance: packaged GeoClue extension

Upstream GeoClue's `Location.Description` is optional human-readable text, not a
source contract. Accuracy does not establish a source either. The daemon never
classifies either one. `packaging/geoclue/` adds the read-only unsigned property
`org.freedesktop.GeoClue2.Location.SourceKind`:

| Value | Meaning | Daemon behavior |
| --- | --- | --- |
| 0 | Unknown | Require IP confirmation |
| 1 | IP-only request or explicit provider `ipf` fallback | No second lookup |
| 2 | Non-IP request/result, including `lacf` cell fallback | Require IP confirmation |
| Missing / unrecognised / wrong type | Unavailable provenance | Require IP confirmation |

This is a Shelllist extension, **not an upstream GeoClue API**. The patch derives
provenance inside GeoClue's Mozilla-compatible request/response adapter, before
human-readable descriptions are assembled. Only a known IP-only request or
explicit `fallback: "ipf"` gets the IP exemption. Unknown/null fallback values
remain unknown. Other adapters currently default to unknown, which deliberately
follows the same confirmation rule as non-IP (including GPS/static sources).
This is provider evidence, not a proof of physical location or provider independence.
GeoClue and the independent IP service can both reflect a VPN's egress location.

The property travels with each location and its copies, not as a global "last
provider" flag. The patch also preserves the original observation timestamp on
Wi-Fi cache hits, instead of refreshing old observations when a client starts.
A GeoClue timestamp can still be provider-response time, not AP scan time; neither
it nor this patch proves database age or fresh independent physical observations.
Patch application and production-parser tests are build gates; incompatible
upstream changes fail the build. See [patch ownership](../packaging/geoclue/README.md).
Using an unpatched package remains conservative but cannot exempt IP fallback.

## Enable / migration

Import `bar-daemon.nixosModules.location-timezone` and set
`services.location-timezone.enable = true`. Optional `package` and
`integrationPackage` overrides retain their existing roles. The module enables
GeoClue, defaults to the patched GeoClue package and beaconDB endpoint, and
registers `location-timezone` for the reserved `automatic-timezoned` UID.
The per-service-account `location-timezone-agent` supplies GeoClue 2.7's required
authorization agent. This agent does not set timezones. NMEA discovery defaults
off but hosts may override provider configuration.

The module removes direct NetworkManager scan authorization: the authority no
longer scans Wi-Fi or sends BSSIDs itself. NetworkManager remains the connectivity
and connection-identity observer. The sole privileged write authorization is
`org.freedesktop.timedate1.set-timezone` for the service account.

The unit retains its account, state directories, sandbox, and conflict with
`automatic-timezoned.service`. It owns `org.laufan.LocationTimezone` on the system
bus to exclude duplicate instances. Do not run another timezone writer. Normal
administrator-authorized NixOS activation is required; editing/building this
checkout does not activate it. No Python runtime is used.

The binary accepts `--state-directory`, `--runtime-directory`, and `--tzdata`,
with the existing systemd/environment defaults. Nix supplies tzdata explicitly.

## Scheduling, cancellation and cooldowns

- Observe NetworkManager FULL/CONNECTED_GLOBAL, active/default connections and
  unique bus owner. New connections debounce for ten seconds. Successful or
  conclusively disagreeing checks stay idle until a new connection/reload.
- Missing evidence retries after 30, 60, 120, 240, then 300 seconds. Connectivity
  flicker preserves backoff/completion; a new connection resets it. Offline,
  sleeping or paused automation does not begin acquisition.
- Each GeoClue attempt uses a dedicated connection/client. It requests CITY
  accuracy, subscribes before Start, and waits past stale/invalid fixes. D-Bus
  calls are bounded to five seconds; the entire two-stage acquisition to 90.
  Cleanup deletes the client on success/error/cancellation, bounded to five
  seconds; dropping the private connection is the final ownership fence.
- Fixes must have valid coordinates, timestamps no earlier than the attempt
  (rounded down to GeoClue's whole-second precision), at most five minutes old,
  and at most five seconds in the future. When confirmation is needed, evidence
  timestamps must be within two minutes of each other.
- IP is requested only after usable GeoClue evidence with a resolved timezone,
  then a fresh network/sleep/control check. HTTP is HTTPS-only, without proxies
  or redirects, with a 15-second timeout and 64 KiB streamed-response limit.
- An observed IP HTTP 429 is persisted before cancellation/return. Retry-After
  accepts seconds/HTTP dates, minimum 60 seconds, default one hour, maximum one
  year. Reload/reconnect/restart cannot bypass it. During IP cooldown, ordinary
  bounded retries may ask GeoClue again: a new GeoClue-IP result can succeed, but
  a non-IP/unknown result cannot issue an independent IP request until expiry.
- Existing `wifi_not_before` cooldowns survive migration and gate GeoClue until
  expiry because the default GeoClue endpoint is the same beaconDB provider.
  This legacy field is not repopulated. GeoClue owns subsequent provider HTTP
  traffic; its D-Bus API does not expose Retry-After to this daemon. No claim is
  made that the authority can persist unobservable GeoClue provider cooldowns.
- Network change, sleep (including a coalesced sleep/resume cycle), reload and
  shutdown cancel acquisition. A detached cleanup task cannot query providers or
  set a timezone. Evidence is rechecked against actual network/control state
  immediately before application.

## Resolution and safe effects

The offline resolver is pinned `tzf-rs`/`tzf-dist`, with tzdata alias
canonicalization. Its simplified boundaries have an upstream-reported ~111m
maximum deviation; nearby ±0.002-degree samples must resolve unambiguously.
Boundary uncertainty is retriable missing evidence, never a permanent
"sources disagree" result. This sampling is not a distance comparison between
providers. See `packaging/licenses/timezone-boundaries.md` for ODbL attribution.

Before applying, reread the system timezone and pause if it differs from the
baseline (manual override). Persist write intent, then recheck connection and
sleep/control state. Call timedated noninteractively only when the canonical
zone differs, and verify readback even when no write was needed. Only durable,
verified outcomes replace accepted state. Lost/rejected replies or verification
failure retain `write_pending` and pause across restarts; there is no blind replay.
SIGTERM allows bounded verification/persistence. `systemctl reload
location-timezone` explicitly clears pause/uncertain intent and captures a new
baseline without bypassing cooldowns.

## State, privacy and presentation

- `/var/lib/location-timezone/state.json`: mode 0600 in a 0700 directory;
  accepted location, `ip_not_before`, legacy `wifi_not_before`, `paused`, and
  `write_pending`. Old Python/Rust state loads without dropping accepted data or
  safety fences; missing provenance becomes `unknown`, missing accuracy `null`.
  Corrupt, oversized or symlinked state fails closed.
- `/run/location-timezone/status.json`: mode 0644, only `{ "message": "..." }`.
  Both files use atomic replacement and parent-directory fsync.
- Authorized `GetAccepted` returns the last accepted timezone, coordinates,
  verification timestamp, `source` (`ip`, `non_ip`, `unknown`) and optional
  `accuracy_meters`. Active local sessions are allowed by polkit; inactive/remote
  sessions are denied without prompting. There is no location mutation API.

The session daemon retains accepted fixes through temporary reader failures and
projects the stable `current-location` row. These are **accepted estimated
coordinates**, potentially coarse IP estimates, not verified physical position.
Rejected/pending evidence never replaces that row. No weather lookup or reverse
geocoding is implied. The bar clock continues to follow observed timedated offset,
not the stored accepted-location zone or Qt's cached process timezone.
Status presentation remains optional; existing `FeedbackSource` composition and
keyboard navigation are unchanged.

`journalctl -u location-timezone.service` reports attempts, source categories,
accuracy, observation age, canonical zones, retry/rejection and verified effects.
The authority does not log coordinates, IP addresses, SSIDs, BSSIDs or raw
responses. GeoClue itself owns its provider configuration and diagnostics;
upstream GeoClue DEBUG logs may contain request/location data. Do not enable
those logs casually or treat them as public status.

## Validation

```sh
../daemon-framework/tools/local-build develop . --command cargo test --bin bar-location-daemon --locked
../daemon-framework/tools/local-build build --attr locationGeoclue . --no-link
../daemon-framework/tools/local-build check .
```

Rust fixtures cover IP exemption, non-IP/unknown confirmation, same-zone distant
locations, nearby cross-zone disagreement, aliases versus equal offsets,
missing/expired evidence, staged request counts, cancellation/deadlines,
provider cooldowns, legacy state, manual override and durable uncertain-write
fences. Private D-Bus tests exercise real GeoClue property signatures, structured
and absent provenance, stale/malformed updates, denial, cleanup and timedated
readback. Local HTTP fixtures retain transport/response/429 bounds. The patched
GeoClue build tests its production parser and copy paths without network access.
Shelllist's Qt clock regression checks live offset changes and date rollover.

No fixture requires live providers or host services. Provider coverage, deployed
service-account authorization and actual network moves require separate acceptance
after authorized activation. Build success is not deployment evidence.

Implementation validation: 18 location binary tests passed (including private
D-Bus and local HTTP fixtures); the library suite passed 143 tests with its one
existing PipeWire-dependent test ignored. Strict Clippy passed for the location
binary and test targets. The patched GeoClue package, including the module's
agent-enabled variant, built and passed its Meson test. NixOS module contract
assertions evaluated successfully; the full product Nix check was not run.
Shelllist's focused Qt clock suite passed all four cases, including setup/cleanup.
No live providers, host timezone writes or service activation were exercised.
