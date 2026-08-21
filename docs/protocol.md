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
- `notifications.invokeAction`
- `notifications.reply`
- `updates.refresh`

`activity.queryRange` requires integer `from_unix_ms` and `to_unix_ms` values and is bounded to 370 days. Todo creation accepts `title`, optional `due_unix_ms`, optional local `due_date` (`YYYY-MM-DD`), and priority 0–9.

Battery methods operate on the native ThinkPad threshold interface. `battery.setThresholds` requires `battery_id`, `start_percent`, and `end_percent` satisfying `0 <= start < end <= 100`; it also enables persistent protection management. `battery.setProtection` and `battery.chargeOnce` accept an optional `battery_id`, defaulting to the primary battery. Charge-once temporarily selects `0–100`, survives daemon restarts, and restores the exact previous range on full charge, unplug, or after 24 hours. `battery.setAlertPolicy` accepts any non-empty subset of `warning_percent`, `critical_percent`, `notify_when_full`, and `auto_power_saver`, with `critical_percent <= warning_percent`. See [`battery.md`](battery.md).

Run `bar-daemon debug protocol-registry` for canonical parameter examples. `media.operation` accepts `play-pause`, `play`, `pause`, `stop`, `next`, and `previous`; an omitted `player_id` uses the daemon's current active-player policy.

## Streams

- `activity.changed`
- `workspaces.changed`
- `media.changed`
- `audio.changed`
- `brightness.changed`
- `battery.changed`
- `power-profile.changed`
- `notifications.changed`
- `notifications.active.changed`
- `updates.changed`
- `timezone.changed`

`activity.changed` is a compact summary containing source health, counts, next event, and world-clock metadata. Clients query event/todo collections with `activity.queryRange`; large collections are intentionally excluded from `BarSnapshot`.

In native mode, `notifications.changed` carries compact count, DND, backend, and history-revision state. `notifications.active.changed` carries the complete bounded active notification collection so clients can recover after lag. History is paginated with `notifications.list` using an optional `before_history_id` cursor and a maximum limit of 200.

A subscription first receives `subscribed` with the current complete domain state. Later events are `changed`; a slow subscriber receives `lagged` and should request `bar.snapshot` to recover all domains atomically.

`workspaces.changed` includes the focused monitor, monitor/workspace summaries, and an optional normalized `active_window` object with title, class, workspace, fullscreen, and floating state. `media.changed` player entries include artwork plus MPRIS duration, observed position, observation time, and playback rate. Clients can animate progress between observations while playing; `Seeked` and property changes publish corrected observations.
