# bar-daemon

Rust status, policy, and action service for a Quickshell desktop bar.

Quickshell owns layout, rendering, animation, input, and system-tray menu hosting. `bar-daemon` owns session/system integration, normalized state, policy, and effects. It exposes `bar-api` version 1 over session D-Bus and through a JSON Lines client suitable for QML `Process` integration.

## Current domains

- Hyprland monitor/workspace snapshots, normalized active-window state, and Lua-aware workspace focus
- MPRIS discovery, deterministic active-player selection, artwork and playback timing, seek correction, and playback actions
- Native PipeWire default-output volume/mute and default-input mute state, with bounded output adjustments
- Backlight discovery, watched sysfs state, and permission-safe bounded adjustment
- UPower battery state, health, cycle count, threshold classification, and deduplicated alerts
- power-profiles-daemon state, driver metadata, degradation state, and validated profile changes
- Resilient SwayNC status subscription with normalized count/DND state and panel actions
- Event-driven NixOS delayed-update readiness across fast and delayed lanes
- systemd-timedated timezone, city, abbreviation, and current UTC offset

## Usage

```sh
bar-daemon daemon
bar-daemon client
bar-daemon debug protocol-registry
bar-daemon debug contract-fixture
```

JSONL example:

```json
{"op":"call","id":"snapshot","method":"bar.snapshot","params":{}}
{"op":"subscribe","id":"subscribe","streams":["workspaces.changed","media.changed"]}
{"op":"call","id":"media","method":"media.operation","params":{"operation":"play-pause"}}
{"op":"call","id":"focus","method":"workspace.focus","params":{"workspace_id":2,"on_current_monitor":true}}
{"op":"shutdown","id":"shutdown"}
```

See [`docs/architecture.md`](docs/architecture.md) for ownership and recovery boundaries and [`docs/protocol.md`](docs/protocol.md) for the complete transport contract.

D-Bus endpoint:

- Bus: `org.laufan.BarDaemon`
- Path: `/org/laufan/BarDaemon`
- Interface: `org.laufan.BarDaemon1`

## Development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
nix flake check
```
