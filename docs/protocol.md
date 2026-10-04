# bar-api v1

Every method response is an envelope:

```json
{"protocol":"bar-api","version":1,"ok":true,"data":{}}
```

Failures use `ok:false` and an `error` object containing stable `code` and human-readable `message` fields.

## JSONL transport

Requests accepted by `bar-daemon client`:

```json
{"op":"call","id":"1","method":"bar.snapshot","params":{}}
{"op":"subscribe","id":"2","streams":["audio.changed"]}
{"op":"cancel","id":"3","request_id":"subscription-1"}
{"op":"shutdown","id":"4"}
```

The client emits correlated `response` records and asynchronous `event` records. Stream events carry the embedded versioned event envelope under `event`. A daemon restart emits `transport-error`; supervisors must restart the client and restore calls and subscriptions.

## Methods

- `bar.snapshot`
- `activity.queryRange`
- `activity.refresh`
- `todos.create`
- `todos.complete`
- `todos.delete`
- `workspace.focus`
- `media.operation`
- `audio.adjust`
- `audio.setMuted`
- `audio.setInputMuted`
- `brightness.adjust`
- `brightness.set`
- `battery.history`
- `battery.setThresholds`
- `battery.setProtection`
- `battery.chargeOnce`
- `battery.setAlertPolicy`
- `powerProfile.set`
- `powerProfile.setBatteryAware`
- `powerProfile.setActionEnabled`
- `notifications.togglePanel`
- `notifications.toggleDnd`
- `notifications.setDnd`
- `notifications.list`
- `notifications.dismiss`
- `notifications.clear`
- `notifications.clearGroup`
- `notifications.snooze`
- `notifications.invokeAction`
- `notifications.reply`
- `updates.refresh`

`activity.queryRange` requires integer `from_unix_ms` and `to_unix_ms` values and is bounded to 370 days. Todo creation accepts `title`, optional `due_unix_ms`, optional local `due_date` (`YYYY-MM-DD`), and priority 0–9. Date-only todos are included when their date overlaps the half-open query interval in the daemon's local timezone; partial days and daylight-saving transitions are supported. Timestamped todos retain instant-based filtering.

A todo-store read or parse failure is exposed in `activity.error` and blocks todo mutations without overwriting the file. Repair or restore the file to recover: refresh and subsequent mutations retry loading it. An initially missing file is a valid empty store; intentionally moving a damaged file aside also permits a fresh store.

`battery.history` returns seven-day samples with both wall-clock `timestamp_ms` and compact `active_time_ms`. Graphs should use `active_time_ms` on the x axis and begin a new path whenever `continuous` is false; this removes suspend, shutdown, and daemon downtime from the displayed timescale. Point `mode` is `charging`, `discharging`, or `holding`, and `active_duration_ms` reports the complete compact range.

Battery methods operate on the native ThinkPad threshold interface. `battery.setThresholds` requires `battery_id`, `start_percent`, and `end_percent` satisfying `0 <= start < end <= 100`; it preserves the current enabled and management state. `battery.setProtection` and `battery.chargeOnce` accept an optional `battery_id`, defaulting to the primary battery. Protection updates may include a complete `start_percent`/`end_percent` pair to change the range and enabled state atomically. Charge-once temporarily selects `0–100`, survives daemon restarts, and restores the exact previous range on full charge, unplug, or after 24 hours. `battery.setAlertPolicy` accepts any non-empty subset of `warning_percent`, `critical_percent`, `notify_warning`, `notify_critical`, `warning_profile`, `critical_profile`, and `notify_when_full`, with `critical_percent <= warning_percent`. Profile actions are `keep-current`, `power-saver`, `balanced`, or `performance`; notification switches do not affect profile actions. Legacy `auto_power_saver` still sets both profile actions together. Recovery uses a fixed 3-percentage-point margin. `powerProfile.resumeAutomatic` clears a manual override; `power_profile.battery_automation` reports the active level, requested profile, status, and any automation error. See [`battery.md`](battery.md).

`battery.setChargingInhibited` durably selects or clears the ThinkPad kernel's `inhibit-charge` behavior. `battery.startCalibration` performs a bounded force-discharge/full-charge cycle and `battery.cancelCalibration` safely restores the pre-calibration thresholds. These methods require an explicit `battery_id` in canonical clients, though the daemon accepts omission as primary-battery compatibility behavior.

The `power-sleep.changed` domain exposes systemd-logind suspend/hibernate capability strings, `PrepareForSleep` state, and current inhibitors. `powerSleep.lock`, `powerSleep.suspend`, and `powerSleep.hibernate` target the current logind session; both sleep actions request a session lock first. See [`power-sleep.md`](power-sleep.md).

Run `bar-daemon debug protocol-registry` for canonical parameter examples. `media.operation` accepts `play-pause`, `play`, `pause`, `stop`, `next`, `previous`, `cycle`, `seek`, `select`, `automatic`, and `set-mode`. `cycle` selects the next discovered MPRIS player without invoking playback. `seek` requires a nonzero `offset_seconds` between -86400 and 86400 and calls the MPRIS relative `Seek` method. Playback and seek operations target `player_id` when supplied and otherwise use the daemon's current active-player policy.

`select` requires a currently discovered `player_id` and pins it without invoking
playback. `automatic` clears the pin. `set-mode` requires a current `player_id`
and `mode` of `automatic`, `tracks` or `seek`; it also invokes no playback.
`media` state adds nullable `pinned_player`; players add `control_mode` and
`content_type` (`unknown`, `music`, `podcast`, `video`). These are additive v1
fields/operations. Unknown mode values and disappeared player IDs are rejected.

Automatic selection tracks observed transitions into playing, not list order:
newest currently playing first, then newest retained player, then deterministic
initial fallback. It cannot infer playback starts preceding discovery. Pins last
until Automatic or player exit. Overrides are per MPRIS ID, daemon-session-local,
and removed on exit; they are not disk preferences. Explicit content metadata and
Spotify track/episode URLs support conservative classification. Audio MIME type
and player identity alone remain unknown; clients should use ±30-second seek for
unknown/video/podcast content unless explicitly overridden. Capabilities remain
independent of mode.

## Compositor preferences

`bar.snapshot` includes `compositor: {available, revision, animations_enabled, error}`;
`compositor.changed` publishes the same state on subscription and changes.
`revision` increases only on changes within the daemon lifetime; clients reject
older snapshot replies and reset their revision fence after transport loss.
`animations_enabled` is null until a successful native Hyprland observation.
Errors mark the state unavailable and retain the last known boolean rather than
silently enabling motion. One shared worker refreshes on startup, config reloads
and event-socket reconnect/disconnect, using the existing Hyprland event listener.
Healthy values are not polled; failed reads retry after five seconds or the next
invalidation. Requests use the framework's bounded native socket transport, never
`hyprctl` subprocesses. This is independent of display-control permissions.

UI environment overrides, animation choice and scoped layer-window rules remain
frontend-owned. Deploy the framework, daemon and frontend together for the new
additive stream; clients must not fall back to their own compositor-option parser.

## Streams

- `compositor.changed`

- `activity.changed`
- `workspaces.changed`
- `media.changed`
- `audio.changed`
- `brightness.changed`
- `battery.changed`
- `power-profile.changed`
- `power-sleep.changed`
- `osd-hardware.changed`
- `notifications.changed`
- `notifications.active.changed`
- `updates.changed`
- `timezone.changed`

`activity.changed` includes `lunar`: an optional, UTC-instant-based mean-synodic-month estimate with a semantic phase ID, fraction, age, rounded illumination percent, evaluation timestamp, method and `approximate: true`. It is refreshed on activity publication (normally once per minute), independently of weather availability. Null means unavailable; clients must not substitute a local estimate.

Each weather record includes optional `solar_noon`: the approximate sunrise/sunset midpoint for the current **IANA-zone local date**, with Unix milliseconds, evaluation timestamp, local date, timezone, offset at the midpoint, method and approximation flag. Missing/polar, invalid, reversed, cross-date and stale-day provider times yield null. Cached weather is revalidated at publication; neither estimate is an ephemeris. Clients retain localized labels, disc masks, sun-arc progress and time formatting (using the supplied solar-noon offset).

`activity.changed` is a compact summary containing source health, counts, next event, and world-clock metadata. Clients query event/todo collections with `activity.queryRange`; large collections are intentionally excluded from `BarSnapshot`.

`power-sleep.changed` includes systemd-logind capabilities, current inhibitors, and the live `PreparingForSleep` state. Inhibitors refresh on logind property changes as well as the recovery poll. Only `what` containing the colon-delimited `sleep` token is relevant to explicit sleep actions; `delay` handlers are normal preparation, not blockers. Lock/sleep requests reject concurrent operations, and Suspend/Hibernate reject a system already preparing to sleep. Confirmed screen locking is still required before invoking sleep; failure responses retain the underlying error chain. `osd-hardware.changed` publishes native LED-class state for Caps Lock, Num Lock, keyboard backlight, and microphone/camera privacy indicators; presentation and timeout policy remain owned by Shelllist.

In native mode, `notifications.changed` carries compact count, DND (including an optional expiry), backend, and history-revision state. `notifications.active.changed` carries the complete bounded unsnoozed notification collection so clients can recover after lag. Active records include a stable group key and the focused source monitor captured at ingress. History is paginated with `notifications.list` using an optional `before_history_id` cursor and a maximum limit of 200. `notifications.clearGroup` dismisses one application/group stack, while `notifications.snooze` suppresses a record until its requested wake time.

A subscription first receives `subscribed` with the current complete domain state. Later events are `changed`; a slow subscriber receives `lagged` and should request `bar.snapshot` to recover all domains atomically.

`workspaces.changed` includes the focused monitor, monitor/workspace summaries, and an optional normalized `active_window` object with title, class, workspace, fullscreen, and floating state. `media.changed` player entries include artwork plus MPRIS duration, observed position, observation time, and playback rate. Clients can animate progress between observations while playing; `Seeked` and property changes publish corrected observations.
