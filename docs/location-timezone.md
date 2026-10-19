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
As an additional conservative policy, active NetworkManager VPN connections
(`Vpn=true`, or type `vpn`, `wireguard`, `tun`) block automatic checks and writes,
including in-flight confirmation and the final write fence. The public reason is
`VPN active · timezone unchanged`, distinct from lack of internet. Disappearing or
unreadable active-connection properties fail closed. This also blocks split-tunnel
connections and cannot detect unmanaged/upstream tunnels or all proxies; it is
not proof of the physical network location.

The property travels with each location and its copies, not as a global "last
provider" flag. The patch also preserves the original observation timestamp on
Wi-Fi cache hits, instead of refreshing old observations when a client starts.
The companion `Client.RefreshSince(t)` extension requests one revalidation per
active owning client. It excludes cached Wi-Fi/IP-fallback entries older than the
requested cutoff and refreshes active web sources even when another app keeps
GeoClue running. It never restamps an observation, introduces no periodic provider
poll, and leaves provider failures as pending evidence.
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
authorization agent. This agent does not set timezones. Desktop demo-agent
autostart defaults off; the dedicated agent's `geoclue-demo-agent` identity is
explicitly whitelisted and its separate agent-enabled package retained. The agent
has read-only system/home isolation and only AF_UNIX access in a private network
namespace. It re-registers when GeoClue returns after idle shutdown. NMEA discovery
defaults off but hosts may override provider configuration. This is not strict
application isolation: NixOS still supplies browser application permissions, and
hosts may explicitly enable desktop agents.

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
  Network polling starts after five seconds and runs as a concurrent future;
  a slow observation cannot block acquisition completion, control cancellation,
  or the deadline. Cleanup deletes the client on success/error/cancellation, bounded to five
  seconds; dropping the private connection is the final ownership fence.
- Fixes must have valid coordinates, timestamps no earlier than the observed
  network epoch (rounded down to GeoClue's whole-second precision), at most five
  minutes old, and at most five seconds in the future. Same-network retries and
  reload do not reset that epoch; reconnect/resume does. Non-IP/unknown GeoClue
  observations must be at most 90 seconds old, leaving room for the independent
  request inside the two-minute evidence-pair window. A stale/pre-network fix
  requests one `RefreshSince` revalidation; the existing 90-second acquisition
  deadline still bounds waiting. An unpatched service may lack this extension;
  stale evidence is never accepted merely because revalidation is unavailable.
- IP is requested only after usable GeoClue evidence with a resolved timezone,
  then a fresh network/sleep/control check. HTTP is HTTPS-only, without proxies
  or redirects, with a 15-second timeout and 64 KiB streamed-response limit.
- An observed IP HTTP 429 is persisted before cancellation/return. Retry-After
  accepts seconds/HTTP dates, minimum 60 seconds, default one hour, maximum one
  year. Reload/reconnect/restart cannot bypass it. If non-IP/unknown evidence
  specifically needs rate-limited confirmation, same-network retries wait until
  the greater of the IP and GeoClue cooldowns. Connectivity flicker preserves
  this delay. A new connection, reload or restart can still ask GeoClue and accept
  a positively identified IP result without spending an IP confirmation request.
  Trade-off: a same-network change to IP provenance is not discovered promptly
  during this wait. The retry gate is structured state, not status-string parsing.
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
providers, nor a check of the location's full reported uncertainty. Coarse IP
estimates can resolve to the wrong side of a border even with an unambiguous
coordinate. No accuracy-circle rejection heuristic is implemented: capped or
sparse samples would not establish a guaranteed physical-location bound. See `packaging/licenses/timezone-boundaries.md` for ODbL attribution.

At startup, compare the canonical system timezone with durable `expected_timezone`
(or the accepted zone for legacy state) and persist a pause on mismatch. First
installation without either baseline cannot identify an earlier manual override.
Before applying, reread the system timezone and pause if its canonical identity
differs from the baseline (manual override). Persist write intent, then recheck connection and
sleep/control state. Call timedated noninteractively only when the canonical
zone differs, and verify readback even when no write was needed. Only durable,
verified outcomes replace accepted state. Lost/rejected replies or verification
failure retain `write_pending` and pause across restarts; there is no blind replay.
SIGTERM allows bounded verification/persistence. `systemctl reload
location-timezone` explicitly clears pause/uncertain intent and captures a new
baseline durably without bypassing cooldowns. That acknowledgement survives a
restart even if no new location has yet been accepted.

## State, privacy and presentation

- `/var/lib/location-timezone/state.json`: mode 0600 in a 0700 directory;
  accepted location, `ip_not_before`, legacy `wifi_not_before`, `paused`, and
  `write_pending`, `expected_timezone`, and monotonic `accepted_revision`. Old Python/Rust state loads without dropping accepted data or
  safety fences; missing provenance becomes `unknown`, missing accuracy `null`.
  Corrupt, oversized or symlinked state fails closed.
- `/run/location-timezone/status.json`: mode 0644, only `message` and
  `accepted_revision` (no coordinates or provider details). The revision advances
  with every durably accepted fix, including same-zone/no-write outcomes, and
  survives restart. Publishing it is not suppressed by an unchanged message.
  Both files use atomic replacement and parent-directory fsync.
- Authorized `GetAccepted` returns the last accepted timezone, coordinates,
  verification timestamp, `source` (`ip`, `non_ip`, `unknown`) and optional
  `accuracy_meters`. Readers map future source strings to `unknown` without
  discarding otherwise valid accepted data; unknown never grants IP trust.
  Active local sessions are allowed by polkit; inactive/remote
  sessions are denied without prompting. There is no location mutation API.

The session daemon retains accepted fixes through temporary reader failures and
projects the stable `current-location` row. These are **accepted estimated
coordinates**, potentially coarse IP estimates, not verified physical position.
Its projection includes `location_source` and optional `accuracy_meters`; the
shared list/detail subtitle labels IP/non-IP/unknown estimates and reported
accuracy without changing identity or adding fields. Reported accuracy is not a
guaranteed radius. Rejected/pending evidence never replaces that row. No weather lookup or reverse
geocoding is implied. The bar clock continues to follow observed timedated offset,
not the stored accepted-location zone or Qt's cached process timezone.
The session caches the observed zone and recomputes its offset/abbreviation and
region IDs locally once a minute (DST included). It reads timedated at startup,
on relevant `PropertiesChanged`, on service-owner appearance, or with bounded
exponential retry after a failed read—not on a permanent ten-second timer.
Idle timedated disappearance does not reactivate it. The two-second public-status
file poll discovers accepted revisions even when an entire checking→accepted
transition falls between polls. Message-only changes are local presentation;
a new revision/reader owner triggers authorized `GetAccepted`, with bounded
failure retries and retention of the last accepted fix. Legacy status without a
revision still refreshes the reader on message changes.

Status presentation remains optional; existing `FeedbackSource` composition and
keyboard navigation are unchanged.

`journalctl -u location-timezone.service` reports attempts, source categories,
accuracy, observation age, canonical zones, retry/rejection and verified effects.
Availability transitions include NetworkManager connectivity/state codes, sleep
and blocked reasons. Identity changes report only which of owner, active
connections or primary connection changed, including across an offline interval;
raw connection identifiers are not logged.
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

The packaged-agent re-registration fixture is opt-in: set `LOCATION_GEOCLUE_AGENT`
to the built `libexec/geoclue-2.0/demos/agent` and run `cargo test --bin
bar-location-daemon packaged_service_agent -- --ignored`. Both its system and
session D-Bus addresses point at a private fixture bus; no desktop agent is used.
The service sandbox is checked through Nix module assertions, not activated by
the fixture.

No fixture requires live providers or host services. Provider coverage, deployed
service-account authorization and actual network moves require separate acceptance
after authorized activation. Build success is not deployment evidence.

Initial implementation validation: 18 location binary tests passed (including private
D-Bus and local HTTP fixtures); the library suite passed 143 tests with its one
existing PipeWire-dependent test ignored. Strict Clippy passed for the location
binary and test targets. The patched GeoClue package, including the module's
agent-enabled variant, built and passed its Meson test. NixOS module contract
assertions evaluated successfully; the full product Nix check was not run.
Shelllist's focused Qt clock suite passed all four cases, including setup/cleanup.
No live providers, host timezone writes or service activation were exercised.

Review follow-up validation: 27 location tests and 147 library tests pass; the
library's existing PipeWire-dependent case remains ignored. The opt-in packaged
agent fixture passes separately with private system/session buses, including
re-registration after owner loss. Strict Clippy passes. Both agent-enabled and
agent-disabled patched GeoClue packages build and pass production parser/cache
checks, and the revised Nix module assertions evaluate. Qt clock and actual
Time/Weather metadata/navigation suites pass (4 and 5 cases including lifecycle).
The full Qt suite passes 333 cases with its existing renderer skip, using the Nix
check's `QML_XHR_ALLOW_FILE_READ=1` fixture setting; strict QML lint passes. Panel
UI review reports no new numeric-role or local-wrapper overrides.
No new package activation or live-provider/timezone acceptance was performed.
