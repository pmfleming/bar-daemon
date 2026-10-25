# bar-daemon

## Shelllist Updates

The desktop update monitor exports a genuine `shelllist-updates` StatusNotifierItem
(with native menu and watcher recovery), reads the worker's bounded per-run ledger,
and sends meaningful outcome notifications. `updates.queryHistory` supplies
revision-fenced, searchable pages; `updates.readLog` supplies a bounded read-only
worker journal or private manual rebuild log. `updates.getOverview` reads the
installed worker's bounded, unprivileged status report for deployment/baseline,
installed versions, successful checks and timer schedules, plus unresolved
history issues. Recovery annotations never rewrite historical failures. See
[worker status](docs/update-worker.md) for deadlines and executable configuration.
The catalog also imports the existing
user-owned `nixos-rebuild/history.jsonl` ledger and live markers under XDG state;
`BAR_DAEMON_REBUILD_STATE_DIR` can override that directory. Explicit rebuild
completion notifies even when unchanged; automatic no-change checks stay silent.
Old successful rebuilds import without a popup backlog. No new update execution
API is exposed. `bar-daemon debug update-history` diagnoses these reads without
sending notifications or changing the running system.

Install the matching Shelllist application for the launcher/icon/viewer and the
matching update-worker with `notificationOwner = "desktop"`. Old/legacy records
do not trigger desktop delivery. `BAR_DAEMON_UPDATE_DESKTOP=0` disables the tray and
publisher for rollback; coordinate the worker's switch back to legacy ownership.
`BAR_DAEMON_SHELLLIST` optionally supplies the absolute launcher executable when
it is not on the user service's PATH. Native notifications have transactional
idempotency keys; external notification backends receive summary-only messages
without a claim of exactly-once delivery or retained action support.

See `../shelllist/docs/updates.md` for behavior, coverage, migration and evidence.


Rust status, policy, and action service for a Quickshell desktop bar.

Quickshell owns layout, rendering, animation, input, and system-tray menu hosting. `bar-daemon` owns session/system integration, normalized state, policy, and effects. It exposes `bar-api` version 2 over session D-Bus and through the retained JSON Lines client.

## Current domains

- Activity summaries and bounded range queries across multiple local ICS files/directories, persistent local todos, and configured world clocks
- Hyprland monitor/workspace snapshots, normalized active-window state, and Lua-aware workspace focus
- MPRIS discovery, observed-playback recency, explicit player pins, conservative content classification and session-local control-mode overrides, artwork/timing, seek correction and playback actions
- Native PipeWire default-output volume/mute and default-input mute state, with bounded output adjustments
- Backlight discovery, watched sysfs state, and permission-safe bounded adjustment
- Native Linux power-supply monitoring, ThinkPad charge protection, recoverable charge-once, health/cycle telemetry, and configurable deduplicated alerts
- power-profiles-daemon state, driver metadata, degradation state, and validated profile changes
- systemd-logind sleep capabilities, active inhibitors, confirmed lock-before-suspend/hibernate actions, shared/separate idle profiles, and read-only ThinkPad sleep diagnostics
- Native Freedesktop notification server with expiry, actions, DND, active state, and persistent SQLite history; resilient SwayNC fallback adapter
- Event-driven NixOS delayed-update readiness and read-only worker job status
- systemd-timedated timezone, city, abbreviation, and current UTC offset

Activity location clocks derive their current offset, abbreviation and map regions
locally from IANA timezones at one projection instant, independently of weather
availability or cached forecast offsets. Invalid/missing zones retain row identity
but publish `clock_unavailable: true`; consumers must not display numeric defaults
as UTC. Weather cache, retry and availability semantics are unchanged.

The package also provides **`bar-location-daemon`**, a separate Rust system service
for GeoClue-first timezone updates and bounded failure retries. Confirmed GeoClue
IP results are accepted directly; other results require matching canonical IP
and GeoClue timezones, with no distance limit. It replaces
the former Python location updater, not the session daemon's read-only timezone
observer. Enable `nixosModules.location-timezone` with
`services.location-timezone.enable = true`; see [location/timezone](docs/location-timezone.md).
The embedded boundary library requires Rust 1.88+ and ships ODbL data attribution.

## Development builds

The committed `flake.lock` intentionally retains remote pins only; local sibling
inputs (`daemonFramework`, `updateWorker`, and their transitive inputs) are resolved
in a disposable snapshot by the workspace build tool. Use:

```sh
local-build develop . --command cargo test --all-targets --locked
local-build check .
```

The source launcher is `../daemon-framework/tools/local-build` (requires Cargo).
Plain `nix develop` may rewrite the checkout lock, while `--no-update-lock-file`
can reject its intentionally missing local nodes. Do not commit generated sibling
revision pins; see [the workspace build contract](../daemon-framework/docs/local-build.md).

## Usage

```sh
bar-daemon daemon
BAR_DAEMON_NOTIFICATION_BACKEND=swaync bar-daemon daemon # temporary fallback
bar-daemon client
bar-daemon debug protocol-registry
bar-daemon debug contract-fixture
bar-daemon debug sleep-diagnostics # read-only kernel/swap/ThinkPad evidence
bar-daemon debug lock-state        # read-only Hyprland lock-notification probe
```

JSONL example:

```json
{"op":"call","id":"snapshot","method":"bar.snapshot","params":{}}
{"op":"call","id":"agenda","method":"activity.queryRange","params":{"from_unix_ms":1767225600000,"to_unix_ms":1769904000000}}
{"op":"subscribe","id":"subscribe","streams":["workspaces.changed","media.changed"]}
{"op":"call","id":"media","method":"media.operation","params":{"operation":"play-pause"}}
{"op":"call","id":"media-cycle","method":"media.operation","params":{"operation":"cycle"}}
{"op":"call","id":"media-rewind","method":"media.operation","params":{"operation":"seek","offset_seconds":-30}}
{"op":"call","id":"focus","method":"workspace.focus","params":{"workspace_id":2,"on_current_monitor":true}}
{"op":"call","id":"protect","method":"battery.setProtection","params":{"enabled":true}}
{"op":"shutdown","id":"shutdown"}
```

See [`docs/architecture.md`](docs/architecture.md) for ownership and recovery boundaries, [`docs/quality-policy.md`](docs/quality-policy.md) for metric baselines, [`docs/battery.md`](docs/battery.md) for ThinkPad setup, policy, and migration details, [`docs/power-sleep.md`](docs/power-sleep.md) for logind and Hyprland sleep integration, [`docs/activity-module-plan.md`](docs/activity-module-plan.md) for the Activity roadmap and configuration, and [`docs/protocol.md`](docs/protocol.md) for the complete transport contract.

D-Bus endpoint:

- Bus: `org.laufan.BarDaemon`
- Path: `/org/laufan/BarDaemon`
- Interface: `org.laufan.BarDaemon1`

## Direct Unix JSONL ingress

The shared listener serves `$XDG_RUNTIME_DIR/bar-daemon.sock`. Runtime directory
and socket must be private `0700`, canonical and owned by this UID; the framework
checks kernel peer identity. Listener failure fails the daemon, also for D-Bus
users. The socket invokes the same exported in-process service boundary, domain
coordinators and state store. It does not proxy D-Bus or start another set of
state monitors. Event destinations are transport-neutral and each subscription
retains its existing independent domain workers and bounded/coalesced sources.

Ownership follows socket lifetime, never a frontend-supplied string. Subscription
loss releases work-area demand and forwarders only for that owner. Interactive
battery operations use that same lifetime rather than attempting to resolve a
socket identity on D-Bus. Cancellation signals authorization and waits for durable
reconciliation; admission remains occupied until the work settles. Acknowledging
cancellation is not rollback. D-Bus ownership uses the shared connection monitor;
foreign operation/status access remains denied.

Shelllist independently selects this listener with `SHELLLIST_BAR_SOCKET=1`.
The bridge remains the default; a rebuild alone does not select the socket.
Matching producer/frontend builds and an already-running daemon are required.
There is no automatic bridge fallback after uncertain dispatch. Atomic read/event
ordering and cached current-state recovery apply on both transports. Startup
activation, maximum payload, sustained load, matched release performance,
installed-session and OS-process-crash acceptance remain open.

Private producer tests exercise actual Unix/D-Bus dispatch and validation parity,
mixed subscriptions, unrelated audio bursts, foreign cancellation and independent
work-area lifetime cleanup. A real-socket test runs the actual battery coordinator
with fake authorization work and proves disconnect retains admission until
reconciliation. No battery helper or hardware mutation is invoked. These are not
full-desktop or native Quickshell transport acceptance tests.

A separate `native-quickshell-socket` Cargo feature enables the explicit
`native_quickshell_bar_socket_acceptance` test. It runs real Quickshell against the
same in-process exported service/listener with private state, HOME/runtime and a
private producer bus; the frontend has nonexistent bus addresses. A test-only
read gate refuses effects before dispatch. Nine cached reads (eight before renewal
and one surviving-peer read), a real publication/read race, subscription renewal,
consumer release, one shared owner and work-area cleanup are checked. No host
monitors or hardware helpers run. This is a debug/private native socket probe,
not a real daemon OS-process crash, sustained-load or performance experiment.

The feature is off by default, explicitly separating this probe from the package's
`--include-ignored` suite. It never silently passes without its inputs. Reproduce
from Shelllist using the command in `../shelllist/tests/README.md`, supplying
`SHELLLIST_SOURCE` and `SHELLLIST_QUICKSHELL`; do not run the QML fixture standalone.

## Atomic state publications

Each stream projection carries `_publication: {epoch, revision, previous_epoch?}`.
The same projection is used by `bar.snapshot`, initial subscription and changed
events, captured with its per-domain clock under the state-store lock. Revisions
are positive exact JavaScript integers; rollover starts a fresh random epoch and
retains its predecessor through coalescing. Unchanged state does not advance;
explicit history invalidations do. Health failures advance the affected domain's
publication too, without stamping previously captured payloads.

Effect receipts remain unchanged. Successful replies containing a domain also
include a separately captured `data.snapshot` for that domain. Do not treat the
original receipt or a nested battery operation outcome as current state.
`theme.resolve` is a pure preview; only `theme.configure` publishes live roles.
Consumers remove `_publication` before interpreting domain-only payloads (notably
the string-valued theme role map). Matching frontend/producer builds are required.

Replacement watches may skip revisions. Bounded battery/sleep/active-notification/
hardware-OSD streams still lose historical edges when lagged; a fresh cached read
recovers only current state and must not replay effects. Tests cover atomic
captured reads, health failure, independent clocks, rollover lineage, coalescing,
receipt reconciliation and transition overflow. This is not periodic liveness,
installed-session, native Quickshell socket or release-performance acceptance.

## Browser media sources

MPRIS content URLs provide offline service labels for YouTube, Vimeo, SoundCloud,
Spotify, Pocket Casts and Audible without replacing the browser's control target.
Only explicit content metadata or unambiguous URL patterns classify content;
unknown sources remain usable. Online lookup is off by default. To fill missing
YouTube title/channel/artwork, set `BAR_DAEMON_YOUTUBE_METADATA=1` in the daemon's
service environment and restart it. This sends recognized video IDs to YouTube
without cookies, page scraping or browser extensions. Playback never waits for
lookup. See [media source metadata](docs/media.md) for additive wire fields,
bounds, caching, supported paths and privacy boundaries.

## Isolated Chromium media apps

Chrome publishes one MPRIS player per browser process, not per app window. Use a
separate `--user-data-dir` for each audio app and export these optional labels in
its launcher before starting Chrome:

```sh
export SHELLLIST_MEDIA_IDENTITY='Pocket Casts'
export SHELLLIST_MEDIA_DESKTOP_ENTRY='com.laufan.pocketcasts'
```

The desktop entry ID is the installed filename without `.desktop`. Both values
must be present; the ID accepts ASCII letters, digits, dots, underscores and
hyphens (starting with a letter/digit), and each value is limited to 128 bytes.
The daemon resolves the browser's actual D-Bus owner PID, reads only that process's
environment for the labels, and never logs or persists process data. If Chromium
has dropped these labels, the daemon also recognizes the isolated Pocket Casts
and Audible launcher convention: `--class=com.laufan.pocketcasts` or
`com.laufan.audible`, a matching absolute `--user-data-dir` ending in
`chrome-web-apps/<class>`, and an HTTPS `--app` URL on `play.pocketcasts.com` or a
supported Audible regional host. All three must agree. Native NUL-separated argv
and Chromium's flattened process title are supported; ambiguous values fail
closed. Shared-profile `--app-id` PWAs and ordinary browser tabs are not inferred.
Missing, invalid or inaccessible hints fall back to the player's own identity.
These are presentation hints, not trusted authorization or playback routing
information. No browser profile contents, cookies or history are read.

Player IDs and controls remain tied to the original MPRIS service. Track titles,
authors/hosts, albums, artwork and seek support come from Chrome/the site's Media
Session implementation; no browser history, cookies, or page scraping is used.
Players may only appear after playback starts. Regular Chrome is unchanged.

## Brightness control

Observation and adjustment share one service and lock, including readback and state
publication. Adjustments use requested sysfs brightness (not lagging hardware
feedback), round to the nearest raw level, and stay between 1% and 100%.

On the default `/sys/class/backlight` root, failed direct writes fall back to
`brightnessctl`, with a two-second timeout and child termination on cancellation.
`BAR_DAEMON_BACKLIGHT_ROOT` overrides the root for **both** observation and control;
any explicit override disables the helper fallback so fixture writes cannot reach
real hardware. The override is read once at service startup.

Run the isolated regression tests with `cargo test brightness`; they use temporary
backlight files and fake helpers, never the host backlight.

## Development

Keep `daemon-framework` beside this checkout; its `crates/shelllist-hyprland`
provides Hyprland IPC. No separate Hyprland checkout is needed. The helper uses
the current tracked worktrees, not persistent local-project pins. All five
daemons must share one framework; do not vendor or revision-pin it.

```sh
../daemon-framework/tools/local-build develop .
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked --all-targets -- --include-ignored
# The dev shell supplies PipeWire and HYPRIDLE_TEST_BIN for private fixtures.
cargo llvm-cov --workspace --all-targets --cobertura --output-path cobertura.xml -- --include-ignored
../daemon-framework/tools/local-build check .
```

Nix package checks and CI run all tests, including the ignored PipeWire and native
idle fixtures. The native test uses a private Wayland display, test-owned D-Bus
configuration, mock logind, and an isolated home; it cannot suspend the host.
The patched Hypridle package is shared with Shelllist, not separately vendored:
see [`packaging/hypridle/README.md`](packaging/hypridle/README.md).
The local-build helper requires new source files to be tracked before snapshotting.
