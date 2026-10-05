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

The daemon claims `org.freedesktop.Notifications` by default. Set `BAR_DAEMON_NOTIFICATION_BACKEND=swaync` only for the temporary compatibility adapter. Native notification state is initialized before Activity and other providers. Active internal producers receive a `NotificationSink`, so battery alerts enter the native engine directly rather than calling back through D-Bus. Activity does not retain an unused sink; reminder integration can request one when implemented. `NotificationService` owns the native expiry/signal tasks or the SwayNC subscription as one cancellable backend lifetime.

### Notification persistence backpressure

The native engine reserves a slot in its bounded SQLite worker queue before
mutating memory. Saturation waits asynchronously rather than dropping saves,
dismissals, or DND changes. Each engine operation batches its writes in order;
a cancelled operation still enqueues already-applied mutations. A stopped
worker rejects new mutations through the API. History queries wait for queue
space and serve as a barrier for earlier completed operations.

Enqueueing is not a SQLite durability acknowledgement: abrupt process termination
can still lose queued work, and database write errors are logged. The queue
protocol prevents overload-induced loss, not disk-failure or crash recovery.

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
| OSD hardware | Linux LED class: hardware-change priority notifications plus a 50 ms cached-file fallback; hotplug rediscovery every 5 s | Normalized state only; no raw input access; Shelllist owns presentation |
| Notifications | Native `org.freedesktop.Notifications` server with SQLite WAL history; optional SwayNC adapter | Bounded ingress, replacement IDs, expiry, DND, actions/replies, compact summary and recoverable active state |
| Updates | Delayed updater state-directory watcher | Complete-lane readiness validation |
| Timezone | systemd-timedated system D-Bus | IANA city, abbreviation, and current offset |

### Hyprland events

The framework owns event-socket framing and reconnection. Bar event intake
forwards geometry and compositor-preference invalidations independently of
workspace queries, so slow command IPC cannot block them. Workspace refreshes
coalesce over a fixed 75 ms window; invalidations during a query trigger a
follow-up refresh. Healthy workspace monitoring is event-driven, with one-second
recovery queries while disconnected or after a failed snapshot. All futures are
owned by the domain monitor and cancelled together at shutdown.

### Interactive audio

A dedicated `bar-pipewire-control` thread owns a persistent PipeWire connection.
Requests are serialized; the first key runs without a collection timer, while
already-queued same-direction repeats may be combined. Direction changes and
mute operations retain their ordering. Defaults and hardware routes are queried
for every operation, and replies contain verified readback, so external mixer
changes and device switches are not overwritten from a stale UI cache.

A failed operation drops the connection. The next request reconnects, but the
failed adjustment/toggle is never replayed because it may already have applied.
The independent monitor remains responsible for changes originating elsewhere.

`cargo test --lib private_pipewire_control -- --ignored --nocapture` launches an
isolated PipeWire server with virtual sink/source devices to check repeated
adjustments, mute, external changes, and disconnection without touching live
audio. It requires the `pipewire` executable (included in the dev shell). CI coverage includes this isolated test with `--include-ignored`.

## Enforced boundaries

`rqlens.toml` enforces Cargo-qualified dependency rules. Protocol metadata cannot depend on API handlers, domain integrations cannot depend on API/daemon/client transport modules, and `StateStore` cannot invoke system-effect modules. Run `rqlens measure architecture-rules --config rqlens.toml` followed by `rqlens check --config rqlens.toml` after changing module dependencies. Measurement alone does not enforce violations. A hard CI gate is pending resolution of generated Wayland references; see [`quality-policy.md`](quality-policy.md).

`api`, `daemon`, and `daemon::tasks` are explicit composition roots. Their fan-out is reviewed against the documented baseline rather than hidden behind metric-only facades. See [`quality-policy.md`](quality-policy.md).

## Transport

The D-Bus API uses JSON payloads so QML can consume the same versioned envelopes through the `bar-daemon client` JSONL bridge. Subscription signals are directed to their calling D-Bus owner, and owner loss terminates and removes the subscription. The JSONL client reports service-owner replacement as a transport failure so its supervisor reconnects and restores subscriptions. Protocol drift is guarded by `test_support/bar-api-v1.json`, registry tests, unit tests, and the Nix build check.

ThinkPad threshold writes cross a separate system-bus boundary. The root helper accepts only `BAT` followed by digits, validates that the target is a battery with both threshold attributes, authorizes the caller with polkit, writes in a firmware-safe order, verifies readback, and rolls back a partial write. The session daemon never writes sysfs directly. See [`battery.md`](battery.md) for the complete policy and recovery model.
