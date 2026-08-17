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
- `workspace.focus`
- `media.operation`
- `audio.adjust`
- `audio.setMuted`
- `brightness.adjust`
- `brightness.set`
- `powerProfile.set`
- `notifications.togglePanel`
- `notifications.toggleDnd`
- `updates.refresh`

Run `bar-daemon debug protocol-registry` for canonical parameter examples.

## Streams

- `workspaces.changed`
- `media.changed`
- `audio.changed`
- `brightness.changed`
- `battery.changed`
- `power-profile.changed`
- `notifications.changed`
- `updates.changed`
- `timezone.changed`

A subscription first receives `subscribed` with the current complete domain state. Later events are `changed`; a slow subscriber receives `lagged` and should request `bar.snapshot` to recover all domains atomically.
