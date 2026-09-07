# Integrated calendar and todo — proposed implementation plan

Status: proposal, not implemented. This extends the existing Activity foundation. If approved, it supersedes the provider ordering in `activity-module-plan.md`: Google and Microsoft precede generic CalDAV, and Firebase sync becomes an explicit domain requirement. It retains the existing decision to keep Activity in `bar-daemon`.

## 1. Product decisions

- Extend Shelllist Activity; do not build another frontend.
- Offer **Agenda / To Do / Both**, with Both as the initial default.
- Selecting a calendar day shows that day's events and relevant tasks.
- Support Google Calendar (including calendars attached to a Gmail account), Outlook calendars through Microsoft Graph, and local ICS files.
- Deliver provider reads first, then explicitly enabled two-way calendar editing.
- Use **Cloud Firestore + Firebase Authentication** for app-owned tasks, lists, personal events, and durable preferences.
- Keep SQLite locally for instant queries, offline changes, sync cursors, and reminders. Cloud storage must not make the desktop dependent on connectivity.
- Use Microsoft To Do as a UX reference, not as an automatic requirement to synchronize Microsoft tasks. Treat Microsoft To Do and Google Tasks adapters as later, separately authorized features.

## 2. Existing foundation and gaps

Inspected:

- `bar-daemon/src/activity/{model,provider,service,ics}.rs`
- `bar-daemon/docs/activity-module-plan.md`
- `shelllist/docs/activity.md`
- `shelllist/activity/{ActivitySchedulePane.qml,ActivityFlow.js}`

Already present:

- Month calendar, selected-day agenda, and basic persistent todos.
- Local ICS file/directory sources with source health and last-known-good behavior.
- Bounded `activity.queryRange`, compact Activity updates, and todo create/complete/delete methods.
- Daemon-owned native notifications with persistent history.

Missing or needing expansion:

- Explicit three-mode navigation, task lists/details/editing, and richer task queries.
- Firebase authentication and synchronization; Google/Microsoft authorization and adapters.
- Writable calendars, mutation queues, revision checking, and conflict presentation.
- Recurrence/exception handling, robust ICS import workflow, and Activity reminder scheduling.
- The existing ICS parser reads a subset of VEVENT, not full recurrence support; do not describe it as complete iCalendar support.
- Date correctness needs attention: `ActivityFlow.js` currently computes the end of a timed-event day by adding 24 hours. Use the next local midnight instead, including 23/25-hour DST days. Review ICS missing-end/duration semantics as well.

## 3. Shelllist interaction design

Keep the existing narrow Activity glance rail. Expand schedule inward for the full calendar/task experience; keep notifications and weather in their existing separate callouts.

```text
[Today]  [< September 2026 >]    [Agenda | To Do | Both]    [+]

Month calendar / filters       Selected day
                               Agenda
Calendar visibility             All-day events
Task lists                      09:00  Team meeting      Outlook
                                14:00  Appointment       Google
                               Tasks for this day
                                [ ] Prepare slides       Due today
                                [ ] Read proposal        Planned today
                               Overdue (collapsible)
```

### View behavior

- **Agenda:** selected-day agenda, with an optional upcoming-days list. All-day events above chronological timed events.
- **To Do:** My Day, Important, Planned, All Tasks, Completed, and user-created lists. A selected-date filter is explicit and removable; changing lists must not leave an invisible date restriction.
- **Both:** selected day's agenda and tasks side by side when space permits, stacked on smaller surfaces.
- Clicking a day in Agenda/Both selects that day. In To Do it activates a visible “On <date>” task filter.
- Preserve selected date, focus, and scroll when switching modes. Sync the preferred mode, not every transient navigation action.
- Mark today and the selected day differently. Use distinct event/task indicators; colors also have text/icon labels.
- Show overdue tasks separately on Today; never move their due dates automatically. Undated tasks remain in Inbox/All Tasks and can be explicitly added to My Day.
- A task appears on its due date and/or planned date, once per selected-day result with reason badges. A deadline is not automatically a calendar appointment.
- “Schedule time” creates a linked calendar time block with explicit calendar/start/end confirmation. Completing the task does not delete the event; deleting the event does not delete the task.
- Quick add offers Task/Event and shows the destination list/calendar. In a date-scoped context, show the prefilled date visibly and allow clearing it.
- Task checkbox completes with undo; clicking the title opens details. Event click opens details, with editing disabled for read-only sources.

### Microsoft To Do-inspired scope

First release: lists, add/edit/delete/complete, importance, notes, checklist steps, due date, planned date/My Day, reminders, search, sorting, and collapsed completed tasks.

My Day is a daily selection, separate from due dates; uncompleted tasks remain in their lists when that day's selection expires. Advanced suggestions are not necessary.

Next: recurring tasks, list ordering, linked time blocks. Define fixed-schedule versus completion-relative repeats and preserve completion history before implementing them.

Defer: shared lists, assignment, attachments, email flag integration, natural-language entry, and full task-provider feature parity.

### Simple-calendar practices to adopt

- Month navigator plus agenda list first; defer a dense draggable week-time grid.
- Obvious Today/previous/next controls and readable date headings.
- Clear start/end times, source identity, all-day/multi-day handling, and overlapping-event visibility.
- Locale-aware week start, date format, and 12/24-hour clock.
- Keyboard navigation, visible focus, accessible labels, and non-color-only status.
- Separate loading, empty, offline/stale, authorization-needed, permission-denied, and failed-write states. Never show a failed refresh as an empty calendar.

## 4. Backend architecture

```text
Shelllist QML
    | existing versioned bar-api (D-Bus / JSONL)
bar-daemon::activity
    |-- query and task/event services
    |-- SQLite cache + durable mutation outbox + reminder jobs
    |-- Firebase sync adapter -------- Firestore / Firebase Auth
    |-- Google Calendar adapter ----- Google Calendar API
    |-- Microsoft adapter ----------- Microsoft Graph
    `-- ICS adapter ----------------- files / directories / later URL feeds
```

**Recommendation: retain `bar-daemon`.** This matches existing ownership and reuses notifications, transport, deployment, and Activity state. Run network work in independent bounded workers with timeouts/backoff; none may block bar updates or notification delivery.

Extract private Activity crates if code size warrants it. Reconsider a separate daemon only when Activity needs a genuinely independent lifecycle or additional clients justify the operational cost. Keep domain logic out of `daemon-framework`.

Proposed internal responsibilities: `storage`, `tasks`, `events`, `sync`, `auth`, `reminders`, and provider modules. Extend the existing load-only provider trait into explicit read, incremental-sync, and mutation capabilities; do not force writable behavior onto ICS feeds.

## 5. Data ownership and cloud storage

There must be one authoritative owner per object:

| Object | Authority | Local/cloud treatment |
| --- | --- | --- |
| App task/list | Firestore | SQLite working copy and offline outbox |
| App personal event | Firestore | SQLite working copy and offline outbox |
| Google/Outlook event | Original provider | SQLite cache; do not duplicate into a second editable Firestore calendar |
| File-backed ICS event | File | Read-only cached source |
| Imported ICS event | Chosen app/provider calendar | Copy only after import confirmation |
| App metadata linking task to provider event | Firestore | Reference account/calendar/event identity, not a duplicate event |
| OAuth refresh credentials | Linux Secret Service | Never plain configuration, QML state, logs, or Firestore documents |

Suggested Firestore layout:

```text
users/{uid}/lists/{listId}
users/{uid}/tasks/{taskId}
users/{uid}/calendars/{calendarId}
users/{uid}/events/{eventId}
users/{uid}/eventLinks/{linkId}
users/{uid}/preferences/{documentId}
```

Core models:

- Task: stable ID, list, title, notes, status, importance, ordered steps, due date or zoned time, planned date, reminder, recurrence, timestamps, revision, deletion tombstone.
- Event: stable internal ID, owner/source, remote ID, calendar, title, description, location, all-day date interval or timed interval with source timezone, series/occurrence identity, revision/ETag, capabilities.
- Source/account: provider, calendars, permissions, sync status, last success, typed errors. Calendar/task capabilities degrade independently.
- Local mutation: operation ID, object ID, owner, base revision, payload, state, attempts, next retry.

Use date-only values for all-day events and date-only task deadlines. Event end dates are exclusive. Stable recurring occurrence identity uses series plus original recurrence identity, not the occurrence's edited start time. Handle Windows/IANA timezone mapping and ICS VTIMEZONE/floating times explicitly.

Firestore is not Firebase Hosting: Hosting is optional for a future web client or sign-in helper. Native Rust must not assume the offline behavior of Firebase's web/mobile SDKs; implement SQLite/outbox behavior explicitly. Start with authenticated Firestore REST and bounded incremental polling; evaluate a resumable listener separately.

Cloud sync needs server-managed change timestamps, deterministic pagination/tie handling, update preconditions, and tombstones. Query changes with an overlap/deduplication strategy; commit cursors only with applied pages. Retain tombstones for a documented period and force a full reconciliation on older clients so deleted items cannot resurrect.

## 6. Accounts, authorization, and synchronization

### Google and Outlook

- Register native/public OAuth clients; system-browser authorization with PKCE/state and provider-supported redirect handling. No embedded browser password collection or shipped client secret.
- Firebase sign-in is separate from permission to read/write Google or Microsoft calendars. Validate the native Firebase sign-in/refresh flow in an early spike.
- Begin with least-privilege read scopes. Request write scopes only when enabling editing.
- List calendars, allow selection, and honor per-calendar permissions. Work/school Microsoft tenants may require admin approval.
- Google: initial sync, pagination, sync tokens, deletions, and expired-token full resync. Do not combine sync tokens with incompatible date filters.
- Microsoft: use calendar-view reads/delta where supported, with cursors scoped to the calendar and time window. Advancing the window requires new coverage, not reuse of an unrelated cursor. Verify shared/delegated calendar behavior during the provider spike.
- Poll while the daemon runs and refresh after reconnect, resume, and successful writes. Provider webhooks are not necessary for the first desktop release.

### Pull and push recommendation

Yes to eventual two-way synchronization, **not on day one**:

1. Read-only integration with cached offline display.
2. Create/edit/delete simple events in writable calendars, with explicit destination and visible pending/failed status.
3. Recurring series/occurrence edits after dedicated tests; hide unsupported “this and following” operations.
4. Invitations, RSVP, attendee changes, and meeting notifications as a separate scope. Do not silently send meeting updates while editing personal fields.

Durable writes must survive crashes and distinguish retryable errors, authorization loss, permission errors, and conflicts. Use stable operation IDs and provider-supported create deduplication/reconciliation; a timeout after remote success must not create a second event. Use conditional updates where supported and preserve conflicting versions for resolution rather than silently overwriting remote edits.

On token expiry/full resync, replace only the remote cache after successful staging; preserve unsent local mutations. Multiple devices sync app data through Firestore and provider data directly with its owner. Keep device-specific sync cursors/reminder delivery state local.

No always-on cloud worker initially: calendar refresh and desktop reminders run while `bar-daemon` runs. Sync while the computer is off or future mobile push would require a separate cloud service, token custody design, and webhook renewal jobs.

## 7. ICS support

Present three distinct actions:

1. **Open/watch file or directory:** existing read-only source, with file watching and refresh.
2. **Import once:** preview events, choose a writable destination, show unsupported properties and recurrence warnings, confirm, then report per-item outcomes.
3. **Subscribe to URL:** later read-only feed using conditional HTTP requests; not two-way calendar sync.

Use a tested parser/recurrence library rather than extending a handwritten parser indefinitely. Validate candidates against real Google/Outlook exports before choosing. Cover UID, SEQUENCE, DTSTAMP, DURATION, RRULE/RDATE/EXDATE, RECURRENCE-ID, cancellations, escaping/folding, timezones, and exclusive all-day end dates. Bound file size and recurrence expansion; preserve or warn on unsupported data rather than silently dropping it.

Import deduplication is scoped to destination and source UID/recurrence identity; do not merge unrelated events merely because titles/times match. Re-import offers skip/update/copy choices. Start with VEVENT; detect VTODO and report it as unsupported until task import is delivered. Treat imported text and URLs as untrusted, and never execute alarms/attachments.

## 8. API and repository work

Keep current methods backward compatible. Proposed additions, finalized through checked contract fixtures:

- `tasks.query`: paginated list/search/smart-list/date filtering, including overdue and undated tasks outside the calendar range.
- `todos.update`, list CRUD, task planning and reminder operations.
- `events.get/create/update/delete` with explicit recurrence scope and revision.
- Source/account list, connect/disconnect, calendar selection, capability and sync-status methods.
- ICS preview/import operations and conflict inspection/resolution.

Retain bounded calendar range queries and compact `activity.changed` invalidations. Extend day summaries with event/task counts rather than shipping whole task lists in every snapshot.

Frontend work belongs in `ActivityBackend.qml`, `ActivityController.qml`, `ActivitySchedulePane.qml`, existing agenda/todo components, and new task/source/detail views. Update `ActivityApi.js` and the shared API fixture with backend changes. Shelllist must not gain persistence, OAuth, provider HTTP, or reminder scheduling.

Migrate `todos.json` transactionally into SQLite with an explicit schema version, backup, preserved IDs/dates/completion state, and idempotent restart behavior. Cloud enablement is a separate explicit upload step with a preview; never automatically publish existing private data.

## 9. Security and operational rules

- Firestore rules restrict each user's documents to their Firebase UID, validate writable fields/types/sizes, and constrain server timestamps/revisions; test cross-user access and malformed writes.
- Never put a Firebase Admin/service-account key in a desktop package. Authenticate Firestore user requests with Firebase ID tokens.
- Keep tokens in Secret Service, redact logs, support revoke/disconnect, and explain retain-cache versus remove-local-data choices.
- Use SQLite/file permissions appropriate for private calendar data; do not claim end-to-end encryption for standard Firestore storage.
- Avoid persisting provider event bodies in Firebase by default, especially work calendars subject to organizational policy.
- Bound sync concurrency, API/Firestore reads, cache growth, and import sizes. Use exponential backoff with jitter and provider retry hints; configure billing alerts and usage limits where available.
- Deliver reminders through the existing native notification engine; use persistent occurrence IDs to avoid repeats after restart. Define resume/missed-reminder policy and note that multiple desktops may each notify unless a designated-device policy is enabled.

## 10. Delivery sequence and acceptance gates

| Phase | Deliverable | Exit criteria |
| --- | --- | --- |
| 0 — Design/spikes | Clickable three-mode mockup; native Firebase auth/Firestore proof; Google/Microsoft auth and calendar discovery; parser/recurrence evaluation | Auth renews across restart without secrets in files; target personal/work accounts tested; view behavior and capabilities agreed |
| 1 — Local daily planner | Three views, richer tasks/lists/details/search, SQLite migration, explicit date semantics | Migration is repeat-safe; tasks survive daemon/UI restart; DST, overdue, undated and My Day tests pass |
| 2 — Cloud app data | Firebase Auth, Firestore rules/indexes, task/list/preference sync, offline outbox | Two clients converge after offline edits/deletes; conflicts are visible; cross-user access is denied |
| 3 — Reliable calendar reads | Google then Microsoft reads, source controls, recurrence, hardened ICS watching/preview | Real exports/recurrences match provider UI; refresh failures retain data; reconnect, pagination and token resets pass |
| 4 — Writable integrated calendar | App-owned calendar, provider CRUD, ICS import, linked task time blocks | Offline writes replay once; permission loss/conflicts recover safely; no accidental meeting invitations |
| 5 — Planner polish | Reminders, recurring tasks, optional ICS URL feeds and week view | Fake-clock/restart/resume tests pass; supported recurrence editing semantics documented |
| Later | Optional Microsoft To Do/Google Tasks sync, CalDAV, sharing, attachments, mobile/web client | Separate capability and privacy review; no assumption of cross-provider feature parity |

Cross-cutting tests: Rust unit/provider-contract tests with recorded or synthetic fixtures, fake clocks, crash-after-remote-success fault injection, Firestore Emulator rules/sync tests, QML lint and presentation/keyboard tests, shared bar-api fixtures, and `nix flake check`. Keep live-provider smoke tests opt-in and credentials outside fixtures.

## 11. Research references and takeaways

Reviewed upstream README/docs and the Merkuro schedule screenshot; these are design references, not a claim that any one project is universally the best calendar.

- [Microsoft To Do](https://www.microsoft.com/en-us/microsoft-365/microsoft-to-do-list-app): My Day, lists, due dates, reminders, steps, and cross-device access. Borrow the task/detail interaction, not the entire feature set.
- [Microsoft Graph todoTask](https://learn.microsoft.com/en-us/graph/api/resources/todotask?view=graph-rest-1.0): task lists, importance, recurrence, checklist items, and linked resources. Do not assume every Microsoft To Do UI feature maps to a public API field.
- [KDE Merkuro](https://github.com/KDE/merkuro): strongest stack-adjacent reference for Qt calendar/task presentation. Its schedule screenshot shows useful date-grouped rows and source filters; recreate appropriate patterns in Shelllist rather than adopting Akonadi or copying licensed code.
- [FullCalendar](https://github.com/fullcalendar/fullcalendar): reference for calendar view/navigation and event interaction conventions. JavaScript library, not a native QML dependency; advanced resource features have separate licensing.
- [React Big Calendar](https://github.com/jquense/react-big-calendar): reference for localization and calendar view layout; not a React migration recommendation.
- [khal](https://github.com/pimutils/khal): simple agenda/calendar interaction and standards-based file workflow; synchronization is delegated to vdirsyncer.
- [Google incremental synchronization](https://developers.google.com/workspace/calendar/api/guides/sync): pagination, sync tokens, deletions, and 410 recovery.
- [Microsoft event delta](https://learn.microsoft.com/en-us/graph/api/event-delta?view=graph-rest-1.0): date-window-scoped change tracking and continuation links.
- [Firestore REST authentication](https://firebase.google.com/docs/firestore/use-rest-api) and [offline persistence](https://firebase.google.com/docs/firestore/manage-data/enable-offline): user-token authorization and the need for our own native offline layer.

## 12. Decisions to confirm before implementation

Recommended defaults:

1. Extend `bar-daemon`, no new daemon.
2. Firestore owns app tasks/personal events; Google/Outlook remain owners of their events.
3. Both is the default view; selected-day tasks use due/planned dates without hiding overdue or undated work.
4. Google and Outlook reads in the first integrated release; provider writes in the following milestone.
5. Microsoft To Do-inspired functionality first; actual Microsoft task synchronization later unless explicitly required.
6. Single-user, potentially multiple desktops; no sharing, mobile client, or always-on cloud calendar worker initially.

The key open questions are whether existing Microsoft To Do tasks must appear immediately, whether Outlook includes organization-managed/shared calendars, and whether cloud background sync or a phone/web client is required from the start. Those answers materially change authorization and scope.
