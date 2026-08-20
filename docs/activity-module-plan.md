# Activity module plan

## Decision

Calendar, todos, reminders, world clocks, and eventually notification ownership live in `bar-daemon` as a modular backend domain. They do not become a separate repository or daemon. Shelllist remains a replaceable presentation client: restarting it must not lose sync state, todos, reminders, or notification history.

The design is a modular monolith. One binary and one versioned API compose small engines with explicit traits and failure boundaries. Calendar network failures must never stall notification handling.

## Product surface

The top bar exposes an Activity calendar module and makes the clock and notification indicator open the Shelllist Activity surface. The surface combines:

- a keyboard-accessible month grid;
- selected-day agenda;
- local and provider-backed todos;
- notification summary/history access and DND;
- configurable world clocks;
- source health and refresh state.

Hyprland is the primary compositor. Quickshell owns per-monitor layer-shell surfaces, visual state, input, locale formatting, and animations. `bar-daemon` owns durable state, policy, providers, retries, recurrence, reminder scheduling, notification protocol semantics, and Hyprland fullscreen/focused-monitor policy.

## Repository structure

The first implementation uses internal Rust modules so the ownership boundary is visible without prematurely splitting the existing package:

```text
src/activity/
├── config.rs       # XDG configuration and source definitions
├── ics.rs          # local iCalendar adapter
├── model.rs        # provider-neutral API models
├── provider.rs     # adapter trait and registry
├── service.rs      # orchestration, range queries, todos, summaries
├── notifications/  # current SwayNC adapter and native-server migration seam
└── mod.rs
```

When a second in-repository consumer appears, provider-neutral code can move into private workspace crates without changing the deployed binary:

```text
crates/activity-core/
crates/activity-providers/
crates/notification-server/
```

Crate extraction is a reuse mechanism, not a service boundary.

## Domain model

Calendar events and todos remain distinct types. An all-day value is represented by a local `YYYY-MM-DD` date and is never inferred from midnight UTC. Timed values carry Unix milliseconds plus their source timezone when known. Recurring provider objects are eventually expanded into stable occurrence IDs before reaching Shelllist.

Sources report health independently. A failed refresh retains that source's last-known-good events and records a typed source error. Compact `ActivityState` belongs in `BarSnapshot`; potentially large event/todo collections are returned by bounded range queries.

## Configuration

The initial configuration is JSON at:

```text
$BAR_DAEMON_ACTIVITY_CONFIG
$XDG_CONFIG_HOME/bar-daemon/activity.json
~/.config/bar-daemon/activity.json
```

Example (also checked in as [`activity.example.json`](activity.example.json)):

```json
{
  "calendar_sources": [
    {
      "id": "personal",
      "name": "Personal",
      "kind": "ics-directory",
      "path": "/home/user/.local/share/calendars/personal",
      "color": "#7aa2f7"
    },
    {
      "id": "holidays",
      "name": "Holidays",
      "kind": "ics-file",
      "path": "/home/user/.local/share/calendars/holidays.ics",
      "color": "#9ece6a"
    }
  ],
  "world_clocks": [
    { "timezone": "America/New_York", "label": "New York" },
    { "timezone": "Asia/Tokyo", "label": "Tokyo" }
  ]
}
```

Local todos are stored atomically at `$BAR_DAEMON_TODO_FILE` or `$XDG_STATE_HOME/bar-daemon/todos.json`. Provider credentials will use Secret Service and will never be written to this configuration.

## API

Additive bar-api v1 methods:

- `activity.queryRange`
- `activity.refresh`
- `todos.create`
- `todos.complete`
- `todos.delete`

Additive stream:

- `activity.changed`

`activity.queryRange` is bounded to 370 days. State streams carry complete compact domain values and preserve the existing subscribed/changed/lagged recovery contract.

## Notification migration

Notification replacement is staged because `bar-daemon` and SwayNC cannot simultaneously own `org.freedesktop.Notifications`.

1. Keep the existing SwayNC status/action adapter while delivering the Activity backend and UI.
2. Add an internal notification engine with persistent history, DND/filter policy, expiry, replacement IDs, close reasons, actions, inline reply, activation tokens, and bounded ingress.
3. Expose native history/toast streams to Shelllist and validate feature parity.
4. Add a runtime backend switch and claim `org.freedesktop.Notifications` only in native mode.
5. Remove the SwayNC runtime dependency after migration.

The native engine will start before network providers and communicate through bounded channels. Calendar and todo reminders enter it through an internal `NotificationSink`; they do not shell out to `notify-send`.

## Provider roadmap

1. **Implemented foundation:** multiple local ICS files/directories, compact state, range query, local todos, world clocks, and Activity UI.
2. File watching, recurrence expansion, reminder scheduler, and SQLite cache.
3. Generic CalDAV with discovery, ETags, sync tokens, VEVENT and VTODO.
4. Native Google Calendar and Google Tasks OAuth.
5. Microsoft Graph calendar/tasks and optional Taskwarrior/Todoist adapters.
6. Writable events, RSVP, meeting actions, snooze, and offline mutation queues.

Providers implement capability traits so read-only sources never advertise writes. Partial provider degradation—for example Calendar working while Tasks is unavailable—must not mark the whole account unavailable.

## Verification

- checked bar-api fixture shared with Shelllist;
- parser tests for timed, timezone-aware, all-day, escaped, and folded iCalendar values;
- range bounds and todo persistence tests;
- provider contract tests as network providers arrive;
- fake-clock reminder and notification expiry tests;
- QML lint plus presentation tests;
- restart tests proving the UI is disposable and backend state survives.
