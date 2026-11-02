# Notification relational storage v2

This supersedes the completed v1 Turso adoption plan. Implementation and tests
remain Rust and QML; the existing daemon API remains the Shelllist boundary.

Completed and qualified with the paired Shelllist cutover. See
[release evidence](../tests/notifications-v2-qualification.md) and
[deployment instructions](notifications-storage.md).

## Stages

1. Record the storage and paired-client cutover contract.
2. Replace JSON payloads, previews and metadata with typed rows and ordered
   action tables. Keep one owner thread and explicit transaction/failure fences.
   Switch the default to `notifications-turso-v2.db`, schema version 2.
3. Qualify round-trips, replacement/dependent-row cleanup, rollback, bounded
   reads, restart/crash behavior and the intentional v1 cutover.
4. Clear obsolete Shelllist session state on reconnect; test empty-store
   reconnection and late replies in Qt. Qualify the matched local source graph.
5. Run final Rust and QML qualification and record release evidence.

Each stage is committed. The schema, writers and readers land together so no
intermediate commit writes a format its readers cannot understand.

## Storage contract

`notifications` owns content, scalar hints, captured identity and historical
timing. Ordered sender/context actions live in child tables; typed previews are
a derived cache. Settings, DND/high-water state and application policies have
typed columns. Once-only keys remain independent of history deletion.

Live popup visibility, popup deadlines and frozen-card interaction state are
not restored from history. Stored notifications never revive a D-Bus conversation.
Replacement preserves history identity and replaces child rows atomically.
Pruning, deletion and transient replacement clean children in the same transaction.

Search remains normalized literal substring search over the latest 5,000
persisted records plus the active overlay. Previews avoid full-body hydration;
selected/page details batch their action reads. SQL handles indexed selection,
ordering and deletion; Rust retains normalization, grouping and lifecycle rules.
Wire JSON and byte-limit accounting remain; persisted JSON blobs do not.

## Cutover

The fresh default ignores both `notifications.sqlite3` and
`notifications-turso-v1.db`, leaving their files untouched. Exact path overrides
must select a fresh path. Reject existing incompatible stores rather than reset
them. History, DND, settings, policies, high-water IDs and once-only keys start
fresh; old events may be delivered again. Reopening v2 retains its data.

Shelllist must discard stale live/catalog state, action ownership and deletion
tokens on daemon disconnect, then accept lower revisions and empty pages after
reconnect. No database reader, migration or fallback is added to QML. Shelllist's
co-development build resolves current sibling sources through the shared
framework's local-build tooling; no local revision pin or new lock is introduced.
No live user store is read, deleted or migrated during development.
