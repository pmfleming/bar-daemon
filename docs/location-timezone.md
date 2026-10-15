# Rust location/timezone authority

`bar-location-daemon` is a **separate system daemon built by this repository**.
It lives in `src/bin/bar-location-daemon/` and shares the Rust HTTP/D-Bus/atomic-file stack with
bar-daemon, but does not depend on the graphical session. The session
`bar-daemon` remains a read-only observer of timedated and of the public status
file. There is only one timezone writer. No Python interpreter, Python DBus
bindings, or Python timezonefinder package is used by the new service.

## Enable / migrate

Import `bar-daemon.nixosModules.location-timezone` and set
`services.location-timezone.enable = true`. The module optionally accepts a
`services.location-timezone.package` override. It replaces the existing
`location-timezone.service` executable with `bar-location-daemon`, retains the
reserved `automatic-timezoned` account/UID and state directory, and conflicts with
`automatic-timezoned.service`. Do not run both writers. A session-independent
GeoClue agent is retained. Home/weather settings are not modified.

The system unit authorizes only `org.freedesktop.timedate1.set-timezone` for that
account. It uses a private state directory, read-only system/home access, no new
privileges and restricted address families. The daemon owns
`org.laufan.LocationTimezone` on the system bus to exclude duplicate Rust instances;
its accompanying D-Bus policy permits only the service account to own that name.
It exposes no location mutation API.

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
- Own a separate GeoClue connection/client per attempt. Subscribe before Start,
  reject cached pre-attempt results, and wait through fallback/imprecise fixes.
  `No WiFi networks found` concerns geolocation scan evidence, **not internet
  connectivity**. The acquisition deadline is 90 seconds; individual D-Bus calls
  and recovery observations are bounded to five seconds.
- Query `https://ipapi.co/json/` independently, with HTTPS only, no proxy or
  redirects, a 15-second request timeout and a 64 KiB streaming response limit.
- Missing evidence, setup failures and timeouts retry after **30, 60, 120, 240,
  then 300 seconds**. The delay remains capped at five minutes while failures
  persist. Connectivity flicker preserves pending backoff; a genuinely new
  connection resets it. Offline/sleeping machines and paused automation do not
  issue new checks.
- Observed HTTP 429 cooldowns are persisted before returning a retry result and
  survive reconnect/restart. Retry-After supports seconds and HTTP dates, with a
  minimum of 60 seconds, default of one hour, and defensive maximum of one year.
  The next attempt waits for both the backoff and provider cooldown. There is no
  local daily request quota. Unobserved replies to cancelled HTTP requests cannot
  establish a cooldown.
- Network change, sleep signals (including a coalesced sleep/resume cycle), reload
  and shutdown cancel the owned acquisition futures. No detached acquisition can
  later apply a timezone. GeoClue client cleanup is bounded and connection-scoped.

## Agreement and effects

A usable GeoClue fix must be labelled `WiFi`, newer than the attempt, and have
reported accuracy in `(0, 25 km]`. Both fixes must be finite, within coordinate
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
  `accepted`, `ip_not_before`, `paused`, and `write_pending`. Existing Python JSON
  is read directly; obsolete `ip_calls`/`budget_day` fields disappear on durable
  save without losing accepted coordinates, pause state or cooldowns. Corrupt,
  oversized or symlinked state fails closed rather than silently resetting policy.
- `/run/location-timezone/status.json`: mode 0644 in a 0755 directory, containing
  only `{ "message": "..." }`. No IP addresses, BSSIDs or coordinates are logged
  or published. Both files use atomic replacement and parent-directory fsync.

`src/timezone.rs` still observes `org.freedesktop.timedate1` PropertiesChanged with
five-minute recovery and polls the bounded public status file every two seconds.
It exposes the warning in `timezone.error` without invalidating a successfully
read system offset. Empty messages clear warnings. Missing optional service files
preserve system-timezone-only behavior. Shelllist uses this observed offset for
its clock and accessible description; no frontend protocol change is necessary.

## Validation

Run `cargo test --bin bar-location-daemon --locked`. Tests cover real embedded
Dublin/Alberta boundaries, stale/fallback/broad fixes, disagreement, retry/backoff,
network/sleep cancellation, restart/cooldown and uncertain-write fences, private
state migration/permissions, failed durable writes, and bounded HTTP transport.
Private `dbus-daemon` fixtures exercise real NM/GeoClue/timedated wire calls,
`Properties.Set(ssv)`, fallback-to-WiFi updates, verified application and cancellation
cleanup. All service names in these tests belong to the fixture's private bus;
HTTP tests use a local listener, not live providers.

The complete fixture suite uses `tools/quality-fixtures.sh test` and includes the
existing PipeWire/hypridle fixtures. Nix package tests also run all targets. Tests
prove policy and adapters under controlled conditions, not current live provider
agreement. Deployment, service-account permissions and live acquisition still need
validation after activation; rebuilding must not be confused with activating.
