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
- `notifications.list` (legacy)
- `notifications.queryHistory`
- `notifications.queryCenter`
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
`content_type` (`unknown`, `music`, `podcast`, `audiobook`, `video`). Players also
carry nullable `source: {url, service}` and `content_type_source`
(`unknown`, `mpris`, `url`). The source URL is validated player-supplied metadata,
not a fetch/open instruction; service keys are nullable presentation hints.
These are additive v1 fields/operations. See [media source metadata](media.md)
for recognition rules, URL privacy and compatibility. Unknown mode values and
disappeared player IDs are rejected.

Automatic selection tracks observed transitions into playing, not list order:
newest currently playing first, then newest retained player, then deterministic
initial fallback. It cannot infer playback starts preceding discovery. Pins last
until Automatic or player exit. Overrides are per MPRIS ID, daemon-session-local,
and removed on exit; they are not disk preferences. Explicit content metadata
takes precedence over conservative URL classification (Spotify tracks/episodes,
YouTube/Vimeo videos).
Other recognized service URLs need not imply a content kind. Audio MIME type
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
Healthy values are not polled while event delivery is connected; failed reads
or disconnected event delivery retry after five seconds or the next invalidation.
Requests use the framework's bounded native socket transport, never `hyprctl`
subprocesses. This is independent of display-control permissions.

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

In native mode, `notifications.changed` carries compact count, DND (including an optional expiry), backend, and history-revision state. `notifications.active.changed` carries the complete bounded unsnoozed notification collection so clients can recover after lag. Active records include a stable group key and the focused source monitor captured at ingress. `toast_visible` and `toast_expires_unix_ms` describe popup presentation separately from `expires_unix_ms` (protocol lifetime). A normal non-transient notification using server-default expiration hides its popup after five seconds but remains live/actionable in the center. Explicit positive client timeouts still close; zero never expires; critical/default popups remain sticky. DND suppresses popups, not retained live entries. Snooze starts a fresh five-second popup window on wake. The existing 200-live-record cap evicts the oldest with close reason `UNDEFINED`, retaining its history. On server restart, old live rows become closed history, not revived actions/popups; persisted ID high-water state includes transient IDs to prevent aliasing archived records. Only the exact `inline-reply` action permits `notifications.reply`; ordinary reply action keys use `invokeAction`.

Group dismissal and expiry publish only the final summary/collection per batch (individual D-Bus close signals are retained). Popup hiding and timed-DND expiry do not increment `history_revision`. History reads wait for the mutation/persistence-enqueue boundary, so a subscriber querying a newly published revision cannot overtake its pending write batch.

`notifications.queryHistory` is the legacy record-page catalog. Request parameters are `{query: "", cursor: null, anchor: null, limit: 50}`. It merges unsnoozed live records with the newest **5,000 persisted records**, deduplicates by `(id, created_unix_ms)`, sorts descending by creation time then ID, and applies Unicode-lowercase literal substring search across app name, summary and body. `%`/`_` are literal, not SQL wildcards. This query scope does not delete older persisted history. Live transient records are included; closed transients and persisted records replaced by transients are absent.

The result `notification_page` contains `epoch`, exact string `revision`, normalized `query`, `records`, nullable `next_cursor`, `scope_limit`, and `anchor_reached`. Each record has `notification`, nullable `history_id`, `closed_unix_ms`, and `close_reason`. Live records use a null history ID; view identity remains notification ID plus creation time. Popup fields are normalized away from live toast changes. A page holds 1–100 requested records (default 50), with at most 512 KiB of serialized record content; byte truncation still returns a progressing cursor. A record exceeding that budget fails explicitly. Queries are limited to 1,024 UTF-8 bytes before and after normalization, and two concurrent reads. The scan covers at most the recent persisted scope plus the existing 200-live-record cap. Only candidate IDs are sorted in SQL before loading bounded payloads.

Cursors are opaque, stateless read positions bound to epoch, content revision and normalized query, expiring after 120 seconds. They grant no authority and retain no snapshots/leases; concurrent clients can page independently. Each read holds the engine mutation boundary through persistence retrieval, so pages cannot carry a revision newer than their stored content. Mutation write failures make new catalog reads fail closed for that worker lifetime. `history-cursor-stale` requires restarting the read; `history-query-invalid`, `history-busy`, and `history-unavailable` are explicit failures. Daemon replacement changes epoch even when the revision resets. No mutation may be replayed as cursor recovery.

For refresh, optionally send the oldest visible notification as `anchor: {created: <created_unix_ms>, id: <id>}` on each page. `anchor_reached` becomes true when the page reaches or passes that position, or exhausts the query—even if that record was deleted. Clients can stage the requested window and atomically replace it without reconstructing a full catalog, losing their viewport, or preserving omitted/deleted records. A new query normally requests only its first page.

The additive API leaves `notifications.list` available for legacy clients: optional `before_history_id` and maximum limit 200, without the new consistency/search guarantees. Storage adds/backfills a Unicode-normalized search column transactionally; legacy payloads and history IDs remain intact. Payload-update invalidation and a dirty-row index repair legacy writes after rollback/re-upgrade, without rescanning unchanged retained payloads on every startup. Deploy matching daemon and frontend builds; do not implement a frontend catalog fallback. `notifications.clearGroup` still dismisses a group, and `notifications.snooze` suppresses a record until wake.

### Grouped notification center

`notifications.queryCenter` supplies native app aggregation and bounded detail
snapshots over the same recent scope, search semantics, mutation boundary and
shared two-reader limit as `queryHistory`. It does not change read status or
notification lifecycle. The response envelope is `notification_center`; every
snapshot has `view`, `epoch`, string `revision` and normalized `query`.

- `{view: "apps", query: "", offset: 0, epoch: null, revision: null,
  app_anchor: null}` returns up to 50 `apps`, `total_apps`, `offset`, nullable
  `next_offset` and `anchor_reached`. Each app has `key`, matching `count`,
  scope-wide `total_count` and its newest matching `latest` preview. Apps sort by
  that record's `(created_unix_ms, id)` descending, never by count. Continuation
  offsets require the returned epoch/revision; a changed revision fails with
  `history-cursor-stale`, not a mixed page. An optional app key anchor lets clients
  privately stage a refresh through their old window before atomic publication.
- `{view: "app", query: "", app_key: <key>, page: 1, page_anchor: null,
  selected: null}` returns `app_key`, matching `count`, `total_count`, `page`,
  `pages`, at most three `overview` previews, five `entries`, and nullable full
  `selected` catalog record. Pages are one-based, bounded to 1–1040; shrinking
  results clamp to the last page (empty is page 1 of 1). `page_anchor:
  {id, created}` resolves the page containing that record when still present;
  otherwise the requested page is clamped. Direct seeking needs no cursor replay.
  `selected: {id, created}` loads only that matching record in the selected app,
  independently of its current index page; a missing/nonmatching record is null.
  With no `app_key`, `group_key` or `selected` can resolve a legacy toast link to
  an app without making the conversation key the app identity.

Preview strings are bounded (name 128, icon 512, summary 160, body 240 Unicode
characters). Search still uses full normalized app/summary/body text. SQLite
projects bounded metadata; Rust overlays unsnoozed live identities and aggregates
all candidates before paging. Only the selected stored record deserializes its
full notification/actions. Each complete response is limited to 512 KiB and fails
explicitly if it cannot fit; no silently truncated selected payload is returned.

App keys prefer desktop entry, otherwise an unambiguous exact name/icon pair.
Unnamed senders and overlong identity components (>1024 UTF-8 bytes, or an encoded
key >4096 bytes) conservatively use creation time plus ID, avoiding accidental
merges and unqueryable keys. These are descriptive grouping keys, not trusted
application identities or mutation targets. Conversation `group_key` remains
independent. Mutations still require the existing live notification/action guards;
archived records never revive old D-Bus actions. Deploy matching frontend and
daemon builds; clients must not reconstruct these groups from loaded record pages.

A subscription first receives `subscribed` with the current complete domain state. Later events are `changed`; a slow subscriber receives `lagged` and should request `bar.snapshot` to recover all domains atomically.

`workspaces.changed` includes the focused monitor, monitor/workspace summaries, and an optional normalized `active_window` object with title, class, workspace, fullscreen, and floating state. `media.changed` player entries include artwork plus MPRIS duration, observed position, observation time, and playback rate. Optional YouTube enrichment adds a default-empty `metadata_sources` map identifying only filled fields (`title`, `artist`, `art_url`) as `youtube-oembed`; supplied MPRIS values are never overwritten. Enriched artwork is a private temporary local-file URL, not a remote fetch instruction. See [media metadata](media.md) for opt-in and cache lifetimes. Clients can animate progress between observations while playing; `Seeked` and property changes publish corrected observations.
