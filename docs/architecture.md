# Architecture

`bar-daemon` follows the same boundary as the Shelllist domain daemons:

- Rust owns system/session integration, normalized state, policy, validation, recovery, and effects.
- Quickshell owns windows, monitor-local layout, visual formatting, tooltips, animation, and pointer/keyboard interaction.
- The system tray remains in Quickshell because it must host StatusNotifierItem icons and DBusMenu objects.
- Network and Bluetooth remain owned by `nm-daemon` and `bt-daemon`; `bar-daemon` does not proxy or duplicate those APIs.

## Runtime

The daemon exports one session D-Bus object and starts one monitor per domain. Monitors normalize upstream changes into `BarSnapshot`. `StateStore` compares complete domain values, suppresses duplicates, and broadcasts only changed domains. Subscribers receive an initial domain value followed by ordered changes.

Unavailable integrations produce a typed domain state rather than terminating the daemon. Monitors reconnect with bounded delays or periodic recovery checks.

## Domain integrations

| Domain | Integration | Policy/effects |
| --- | --- | --- |
| Workspaces | Direct Hyprland command/event sockets | Positive IDs, Lua-aware focus dispatch |
| Media | MPRIS session D-Bus | Playing → Spotify → controllable → first selection |
| Audio | Native PipeWire registry, metadata, and node properties | Cubic/linear conversion, 0–100% clamp |
| Brightness | sysfs discovery and file watching | 1–100% clamp, direct write with `brightnessctl` permission fallback |
| Battery | UPower system D-Bus with sysfs fallback | Warning/critical/full deduplication per power cycle |
| Power profile | power-profiles-daemon system D-Bus | Available-profile validation |
| Notifications | Long-lived SwayNC Waybar subscription | Restart backoff and normalized count/DND state |
| Updates | Delayed updater state-directory watcher | Complete-lane readiness validation |
| Timezone | systemd-timedated system D-Bus | IANA city, abbreviation, and current offset |

## Transport

The D-Bus API uses JSON payloads so QML can consume the same versioned envelopes through the `bar-daemon client` JSONL bridge. Subscription signals are directed to their calling D-Bus owner, and owner loss terminates and removes the subscription. The JSONL client reports service-owner replacement as a transport failure so its supervisor reconnects and restores subscriptions. Protocol drift is guarded by `test_support/bar-api-v1.json`, registry tests, unit tests, and the Nix build check.
