# Architecture

`bar-daemon` follows the same boundary as the Shelllist domain daemons:

- Rust owns system/session integration, normalized state, policy, validation, recovery, and effects.
- Quickshell owns windows, monitor-local layout, visual formatting, animation, and pointer/keyboard interaction.
- The system tray remains in Quickshell because it must host StatusNotifierItem icons and DBusMenu objects.
- Network and Bluetooth remain owned by `nm-daemon` and `bt-daemon`; `bar-daemon` does not proxy or duplicate those APIs.
- Activity is an internal modular domain: calendars, todos, reminders, world clocks, and notification policy share one process while retaining independent engines and failure boundaries. See [`activity-module-plan.md`](activity-module-plan.md).

## Runtime

The daemon exports one session D-Bus object and starts one monitor per domain. Monitors normalize upstream changes into `BarSnapshot`. `StateStore` compares complete domain values, suppresses duplicates, and broadcasts only changed domains. Subscribers receive an initial domain value followed by ordered changes.

Unavailable integrations produce a typed domain state rather than terminating the daemon. Monitors reconnect with bounded delays or periodic recovery checks.

The daemon claims `org.freedesktop.Notifications` by default. Set `BAR_DAEMON_NOTIFICATION_BACKEND=swaync` only for the temporary compatibility adapter. Native notification state is initialized before Activity and other providers. Internal producers receive a `NotificationSink`, so battery alerts and future reminders enter the native engine directly rather than calling back through D-Bus.

## Domain integrations

| Domain | Integration | Policy/effects |
| --- | --- | --- |
| Activity | Local ICS sources and XDG todo state; provider adapters are staged behind normalized models | Bounded range queries, last-known-good source state, todo validation, world clocks |
| Workspaces | Direct Hyprland command/event sockets | Positive IDs, active-window normalization, Lua-aware focus dispatch |
| Media | MPRIS session D-Bus | Playing → Spotify → controllable → first selection; artwork and timing normalization |
| Audio | Native PipeWire default sink/source metadata and node properties | Output cubic/linear conversion and 0–100% clamp; output and microphone mute |
| Brightness | sysfs discovery and file watching | 1–100% clamp, direct write with `brightnessctl` permission fallback |
| Battery | Native power-supply sysfs plus udev events; temporary opt-in UPower adapter | Energy-weighted multi-battery telemetry, persistent alert policy, ThinkPad thresholds, crash-safe charge-once recovery |
| Power profile | Standard Power Profiles system D-Bus (`power-profiles-daemon`, `tuned-ppd`, or `tlp-pd`) | Available-profile validation, optional battery-aware/actions capability discovery, active holds and degradation state |
| Notifications | Native `org.freedesktop.Notifications` server with SQLite WAL history; optional SwayNC adapter | Bounded ingress, replacement IDs, expiry, DND, actions/replies, compact summary and recoverable active state |
| Updates | Delayed updater state-directory watcher | Complete-lane readiness validation |
| Timezone | systemd-timedated system D-Bus | IANA city, abbreviation, and current offset |

## Enforced boundaries

`rqlens.toml` enforces Cargo-qualified dependency rules. Protocol metadata cannot depend on API handlers, domain integrations cannot depend on API/daemon/client transport modules, and `StateStore` cannot invoke system-effect modules. Run `rqlens measure architecture-rules --config rqlens.toml` after changing module dependencies.

`api`, `daemon`, and `daemon::tasks` are explicit composition roots. Their fan-out is reviewed against the documented baseline rather than hidden behind metric-only facades. See [`quality-policy.md`](quality-policy.md).

## Transport

The D-Bus API uses JSON payloads so QML can consume the same versioned envelopes through the `bar-daemon client` JSONL bridge. Subscription signals are directed to their calling D-Bus owner, and owner loss terminates and removes the subscription. The JSONL client reports service-owner replacement as a transport failure so its supervisor reconnects and restores subscriptions. Protocol drift is guarded by `test_support/bar-api-v1.json`, registry tests, unit tests, and the Nix build check.

ThinkPad threshold writes cross a separate system-bus boundary. The root helper accepts only `BAT` followed by digits, validates that the target is a battery with both threshold attributes, authorizes the caller with polkit, writes in a firmware-safe order, verifies readback, and rolls back a partial write. The session daemon never writes sysfs directly. See [`battery.md`](battery.md) for the complete policy and recovery model.
