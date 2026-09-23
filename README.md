# bar-daemon

Rust status, policy, and action service for a Quickshell desktop bar.

Quickshell owns layout, rendering, animation, input, and system-tray menu hosting. `bar-daemon` owns session/system integration, normalized state, policy, and effects. It exposes `bar-api` version 1 over session D-Bus and through a JSON Lines client suitable for QML `Process` integration.

## Current domains

- Activity summaries and bounded range queries across multiple local ICS files/directories, persistent local todos, and configured world clocks
- Hyprland monitor/workspace snapshots, normalized active-window state, and Lua-aware workspace focus
- MPRIS discovery, deterministic active-player selection, artwork and playback timing, seek correction, and playback actions
- Native PipeWire default-output volume/mute and default-input mute state, with bounded output adjustments
- Backlight discovery, watched sysfs state, and permission-safe bounded adjustment
- Native Linux power-supply monitoring, ThinkPad charge protection, recoverable charge-once, health/cycle telemetry, and configurable deduplicated alerts
- power-profiles-daemon state, driver metadata, degradation state, and validated profile changes
- systemd-logind sleep capabilities, active inhibitors, confirmed lock-before-suspend/hibernate actions, shared/separate idle profiles, and read-only ThinkPad sleep diagnostics
- Native Freedesktop notification server with expiry, actions, DND, active state, and persistent SQLite history; resilient SwayNC fallback adapter
- Event-driven NixOS delayed-update readiness across fast and delayed lanes
- systemd-timedated timezone, city, abbreviation, and current UTC offset

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
{"op":"call","id":"media-rewind","method":"media.operation","params":{"operation":"seek","offset_seconds":-15}}
{"op":"call","id":"focus","method":"workspace.focus","params":{"workspace_id":2,"on_current_monitor":true}}
{"op":"call","id":"protect","method":"battery.setProtection","params":{"enabled":true}}
{"op":"shutdown","id":"shutdown"}
```

See [`docs/architecture.md`](docs/architecture.md) for ownership and recovery boundaries, [`docs/quality-policy.md`](docs/quality-policy.md) for metric baselines, [`docs/battery.md`](docs/battery.md) for ThinkPad setup, policy, and migration details, [`docs/power-sleep.md`](docs/power-sleep.md) for logind and Hyprland sleep integration, [`docs/activity-module-plan.md`](docs/activity-module-plan.md) for the Activity roadmap and configuration, and [`docs/protocol.md`](docs/protocol.md) for the complete transport contract.

D-Bus endpoint:

- Bus: `org.laufan.BarDaemon`
- Path: `/org/laufan/BarDaemon`
- Interface: `org.laufan.BarDaemon1`

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

Keep `daemon-framework` and `shelllist-hyprland` beside this checkout. The helper
uses their current tracked worktrees, not persistent local-project pins. All five
daemons must share one framework; do not vendor or revision-pin it.

```sh
python3 ../daemon-framework/tools/local-build.py develop .
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked --all-targets -- --include-ignored
# The dev shell supplies PipeWire and HYPRIDLE_TEST_BIN for private fixtures.
cargo llvm-cov --workspace --all-targets --cobertura --output-path cobertura.xml -- --include-ignored
python3 ../daemon-framework/tools/local-build.py check .
```

Nix package checks and CI run all tests, including the ignored PipeWire and native
idle fixtures. The native test uses a private Wayland display, test-owned D-Bus
configuration, mock logind, and an isolated home; it cannot suspend the host.
The patched Hypridle package is shared with Shelllist, not separately vendored:
see [`packaging/hypridle/README.md`](packaging/hypridle/README.md).
The local-build helper requires new source files to be tracked before snapshotting.
