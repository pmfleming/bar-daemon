# Rust location/timezone authority

`bar-location-daemon` is a **separate system daemon built by this repository**.
It lives in `src/bin/bar-location-daemon/` and shares the Rust HTTP/D-Bus/atomic-file stack with
bar-daemon, but does not depend on the graphical session. The session
`bar-daemon` remains a read-only observer of timedated, the public status
file, and the authority's authorized accepted-location reader. There is only one
timezone writer. No Python interpreter, Python DBus bindings, or Python
timezonefinder package is used by the new service.

## Enable / migrate

Import `bar-daemon.nixosModules.location-timezone` and set
`services.location-timezone.enable = true`. The module optionally accepts a
`services.location-timezone.package` override. It replaces the existing
`location-timezone.service` executable with `bar-location-daemon`, retains the
reserved `automatic-timezoned` account/UID and state directory, and conflicts with
`automatic-timezoned.service`. Do not run both writers. The authority no longer
uses GeoClue or its agent; the module does not configure GeoClue for other apps.
Home/weather settings are not modified. Hosts needing GeoClue for browsers or
other applications must enable `services.geoclue2.enable = true` explicitly;
`enableNmea = false` alone does not enable it. Otherwise remove obsolete host
settings/comments that describe GeoClue as a timezone dependency.

The system unit authorizes only `org.freedesktop.timedate1.set-timezone` and
`org.freedesktop.NetworkManager.wifi.scan` for that account (not network editing).
It uses a private state directory, read-only system/home access, no new privileges
and restricted address families. The daemon owns
`org.laufan.LocationTimezone` on the system bus to exclude duplicate Rust instances;
its accompanying D-Bus policy permits only the service account to own that name.
It exposes no location mutation API. `GetAccepted` on
`/org/laufan/LocationTimezone` returns only the last durably verified fix (JSON
or `null`). The `org.laufan.location-timezone.read` polkit action allows active
local sessions by default and denies inactive/remote sessions, without prompting.
The location integration package installs both D-Bus and polkit policies.

NixOS activation restarts the **same** unit, replacing the Python process, rather
than starting a second writer. A source checkout change alone does not activate
it. Use the normal NixOS rebuild/switch workflow with administrator authorization.
The default package installs `bar-location-daemon` alongside `bar-daemon` and
`bar-battery-helper`; enabling the session daemon alone does not enable location.

The binary accepts `--state-directory`, `--runtime-directory` and `--tzdata`;
defaults are systemd's `STATE_DIRECTORY`/`RUNTIME_DIRECTORY`, `TZDIR`, or their
standard system paths. Nix packaging provides tzdata explicitly.

## Acquisition and retry policy

- Observe NetworkManager's FULL/CONNECTED_GLOBAL state and active/default
  connection identity, including its unique D-Bus owner. New connections debounce
  for ten seconds. A five-second recovery poll does not recheck successful or
  conclusively rejected connections merely because time passed.
- Request a fresh NetworkManager scan on managed Wi-Fi devices. Wait for
  `LastScan` to advance past the request on CLOCK_BOOTTIME; read AP `LastSeen`
  on that same clock. Never substitute a cached scan after failure/denial.
  Per-device scans are bounded to 45 seconds, the whole scan phase to 50 seconds,
  and acquisition to 90 seconds. Individual D-Bus calls are bounded to five seconds.
- Drop locally administered, multicast/broadcast, zero/malformed BSSIDs, unnamed
  or `_nomap` SSIDs, weak signals, and stale/future observations. `LastSeen` is
  whole seconds: an observation from the request's partial first second is
  conservatively excluded. APs must be seen after the request and within 60 seconds
  of sending. Deduplicate BSSIDs across devices and send at most 100 strongest APs;
  fewer than two eligible APs means unavailable, not permission for IP fallback.
- POST to `https://api.beacondb.net/v1/geolocate` with `considerIp: false` and
  `fallbacks: {ipf: false, lacf: false}`. Send BSSID, observation age and estimated
  signal strength only, never SSIDs or cell towers. NM supplies quality percentages;
  invert its approximate -100..-40 dBm mapping, not a claim of raw RSSI. Reject
  explicit fallback/error responses, invalid coordinates and inaccurate results.
  A 404 is the distinct result **Wi-Fi location not recognised**. It stops checks
  for this connection rather than periodically retrying missing database coverage.
  A new connection or explicit reload re-arms acquisition; connectivity flicker
  does not. After adding coverage (e.g. with NeoStumbler), explicitly reload to
  try again. Other transport/scan failures still use bounded retry backoff.
- Observation time is the scan request's wall-clock time (a conservative lower
  bound on accepted AP observations), not HTTP reply time. Logs report observation
  age separately from completion. This guarantees a new scan/request, **not** a
  current provider database or independent physical APs behind multiple radios.
- Query `https://ipapi.co/json/` only after a usable Wi-Fi fix exists, rechecking
  connection identity and sleep/control state before starting the IP request.
  Missing coverage, scan failures, invalid fixes and Wi-Fi cooldowns never spend
  an IP request. Both stages share the 90-second acquisition deadline and existing
  120-second evidence agreement window. Both HTTP providers use HTTPS
  only, no proxy or redirects, a 15-second request timeout and a 64 KiB streaming
  response limit. Wi-Fi input filtering and disabling IP fallback are deliberate
  safeguards against ambiguous mesh identifiers and correlated IP-only evidence.
- Missing evidence, setup failures and timeouts retry after **30, 60, 120, 240,
  then 300 seconds**. The delay remains capped at five minutes while failures
  persist. Connectivity flicker preserves pending backoff; a genuinely new
  connection resets it. Offline/sleeping machines and paused automation do not
  issue new checks.
- Observed HTTP 429 cooldowns for each provider are persisted separately before
  returning a retry result and survive reconnect/restart/reload. Neither provider
  is queried while either cooldown is active. Retry-After supports seconds and
  HTTP dates, with a minimum of 60 seconds, default of one hour, and defensive
  maximum of one year.
  The next attempt waits for both the backoff and provider cooldown. There is no
  local daily request quota. Unobserved replies to cancelled HTTP requests cannot
  establish a cooldown.
- Network change, sleep signals (including a coalesced sleep/resume cycle), reload
  and shutdown cancel the owned acquisition futures. No detached acquisition can
  later apply a timezone. Dropping the scan/HTTP futures leaves no detached lookup.
  An already requested radio scan may finish in NetworkManager, but cannot trigger
  a provider request or timezone write from the cancelled attempt.

## Agreement and effects

A usable Wi-Fi fix must come from the filtered Wi-Fi-only request, have an
observation time no earlier than the attempt, and reported accuracy in `(0, 25 km]`.
The internal `WiFi` source label is assigned by this adapter, not inferred from
GeoClue's human-readable description. Both fixes must be finite, within coordinate
bounds, no more than five minutes old, no more than five seconds in the future,
at most two minutes apart, and within 50 km of each other. Both coordinates and
the IP-reported timezone must resolve to the same canonical IANA zone. The IP
country must match tzdata's `zone.tab` entry for that zone. This is a conservative
country/zone consistency check, not a country-polygon lookup or cryptographic
proof of independent providers.

Boundary lookup uses the offline **tzf-rs/tzf-dist** dataset pinned in Cargo.lock,
not Python timezonefinder. Its lite boundaries are simplified (upstream reports
about 111 metres maximum deviation). Nearby ±0.002-degree samples must resolve
unambiguously to the same zone. That rejects some borderline cases but is not a
proof of exact-border equivalence to the previous land dataset. Ocean/unknown
zones fail country consistency. tzdata aliases such as `Eire` canonicalize before
comparison. See `packaging/licenses/timezone-boundaries.md` for data provenance
and ODbL attribution. Rust 1.88 or newer is required by the boundary library.

Confirmed disagreement retains the existing timezone and waits for a new
connection or explicit recovery; there is **no IP-only fallback**. Before a write,
re-read the system timezone and reject manual changes, persist write intent,
then recheck actual network/sleep/control state. Call timedated without interactive
authorization and verify readback. Only verified results become accepted state.
UTC/NTP is never changed.

A lost/rejected write reply or failed verification leaves a durable `write_pending`
fence and pauses automation. Restart cannot blindly retry an uncertain effect.
SIGTERM allows bounded readback/persistence before exit. `systemctl reload
location-timezone` (SIGHUP) explicitly clears pause/uncertain intent, captures a
new baseline, and schedules a new debounced check, **without bypassing cooldowns**.
This explicit recovery is intentionally stronger than the old Python reload,
which could leave an already-attempted connection permanently consumed.

## Files and UI contract

- `/var/lib/location-timezone/state.json`: mode 0600 in a 0700 directory, with
  `accepted`, `ip_not_before`, `wifi_not_before`, `paused`, and `write_pending`.
  Missing `wifi_not_before` in older state defaults to zero. Existing Python JSON
  is read directly; obsolete `ip_calls`/`budget_day` fields disappear on durable
  save without losing accepted coordinates, pause state or cooldowns. Corrupt,
  oversized or symlinked state fails closed rather than silently resetting policy.
- `/run/location-timezone/status.json`: mode 0644 in a 0755 directory, containing
  only `{ "message": "..." }`. No IP addresses, BSSIDs or coordinates are logged
  or published in this public file. Both files use atomic replacement and
  parent-directory fsync. Accepted coordinates are available only through the
  authorized D-Bus reader and the user's session-daemon state.

`src/timezone.rs` still observes `org.freedesktop.timedate1` PropertiesChanged with
ten-second recovery (including accepted-location reads) and polls the bounded
public status file every two seconds. It exposes the warning in `timezone.error`
without invalidating a successfully read system offset. Empty messages clear
warnings. Missing optional service files preserve system-timezone-only behavior.
Shelllist uses this observed offset for its clock and accessible description.

The optional `timezone.accepted_location` contains the last verified timezone,
Wi-Fi coordinates and verification timestamp. Temporary reader failures retain the last
known accepted fix. `activity.locations` projects it as one stable
`current-location` row labelled **Current location**, with `current: true` and
map coordinates. Subsequent accepted fixes replace that row, including fixes
that do not require a timezone write. Rejected fixes never replace it. Existing
saved weather/home/world-clock locations and configuration remain untouched;
Current location is not relabelled Home and does not imply a weather lookup or
reverse-geocoded city name. The existing Shelllist location-list renderer needs
no change. Until a fix is accepted, the previous local-timezone fallback remains.

## Diagnostic logging

Use `journalctl -u location-timezone.service` to inspect attempts. Each check has
an `attempt` timestamp in its tracing span. INFO events report eligible AP counts,
Wi-Fi acquisition (source, accuracy, observation age), provider HTTP statuses,
evidence ages/time separation, Wi-Fi and IP coordinate-derived canonical zones,
IP-reported zone/country, and distance. Rejections identify boundary uncertainty, freshness failure,
distance over 50 km, invalid source, coordinate-zone mismatch, reported-zone
mismatch or country mismatch. These diagnostics **do not change agreement rules**.

Final events distinguish retry (with delay), rejection (waiting for a new
connection/reload), and durable acceptance. Timezone application logs include
current/proposed/verified zones and whether a write was needed. Provider failures
are logged without raw HTTP payloads or request URLs. Exact coordinates, public
IP addresses, SSIDs, BSSIDs and raw HTTP payloads are not logged. DEBUG scan
failure events do not identify networks. Wi-Fi identifiers are sent only to the
configured fixed beaconDB endpoint, not to the IP provider.

## Validation

Run `cargo test --bin bar-location-daemon --locked`. Tests cover real embedded
Dublin/Alberta boundaries, stale/fallback/broad fixes, disagreement, retry/backoff,
network/sleep cancellation, restart/cooldown and uncertain-write fences, private
state migration/permissions, failed durable writes, and bounded HTTP transport.
Private `dbus-daemon` fixtures exercise real NM/timedated wire calls, scan request
serialization, waiting for new `LastScan`, stale/ambiguous/opted-out/duplicate AP
rejection, scan denial, cancellation and verified application. Local HTTP fixtures
check Wi-Fi-only request fields, response validation, distinct 404/no-coverage,
429 responses, redirects, size bounds and deadlines. Authority tests verify
persisted cooldowns, sequential Wi-Fi-before-IP execution, no IP request on
Wi-Fi failure/404, connection-scoped no-coverage stops, reload/new-connection
re-arming and cancellation/timeouts during the IP stage. No test uses live providers
or host system services; successful fixtures do not prove coverage at deployment.

The complete fixture suite uses `tools/quality-fixtures.sh test` and includes the
existing PipeWire/hypridle fixtures. Nix package tests also run all targets. Tests
prove policy and adapters under controlled conditions, not current live provider
agreement. Deployment, service-account permissions and live acquisition still need
validation after activation; rebuilding must not be confused with activating.
