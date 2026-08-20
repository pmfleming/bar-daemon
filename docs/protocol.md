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
- `powerProfile.set`
- `notifications.togglePanel`
- `notifications.toggleDnd`
- `updates.refresh`

`activity.queryRange` requires integer `from_unix_ms` and `to_unix_ms` values and is bounded to 370 days. Todo creation accepts `title`, optional `due_unix_ms`, optional local `due_date` (`YYYY-MM-DD`), and priority 0–9.

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
- `updates.changed`
- `timezone.changed`

`activity.changed` is a compact summary containing source health, counts, next event, and world-clock metadata. Clients query event/todo collections with `activity.queryRange`; large collections are intentionally excluded from `BarSnapshot`.

A subscription first receives `subscribed` with the current complete domain state. Later events are `changed`; a slow subscriber receives `lagged` and should request `bar.snapshot` to recover all domains atomically.

`workspaces.changed` includes the focused monitor, monitor/workspace summaries, and an optional normalized `active_window` object with title, class, workspace, fullscreen, and floating state. `media.changed` player entries include artwork plus MPRIS duration, observed position, observation time, and playback rate. Clients can animate progress between observations while playing; `Seeked` and property changes publish corrected observations.
