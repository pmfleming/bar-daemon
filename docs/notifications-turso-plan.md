# Notifications: fast cutover to Turso 0.8.3

## Scope and decisions

Replace notification persistence using `rusqlite` with the already-pinned embedded `turso = "=0.8.3"`, `default-features = false`.

**Move fast and break storage compatibility. Existing notifications do not need to survive.** No legacy reader, importer, dual-write period, rollback compatibility, or migration framework. Old DND state, settings, per-app policies, ID high-water marks and once-only delivery keys may reset too. Resetting delivery keys can allow an upstream producer to deliver an old event again; that is an accepted cutover consequence.

All new implementation and tests are Rust. QML remains presentation and command dispatch only; no new JavaScript modules, Python, shell migration scripts, C++, or external database CLI. SQL is limited to storage statements embedded in Rust, not application rules.

**Implemented in five staged commits.** Operational details and qualification results are recorded in [notification storage and cutover](notifications-storage.md). The sections below retain the implementation checklist.

## Current code to retain

- `src/activity/notifications/persistence.rs`: bounded 1,024-command worker queue, reservation-before-mutation, ordered batches, cancellation-safe `PendingWrites::drop`, request/reply API, failed-write revision fence.
- `engine.rs`, `history.rs`, `center.rs`, `timeline.rs`, `policy.rs`, `settings.rs`, `identity.rs`, and the other notification domain modules: lifecycle, search normalization, grouping, identity, retention policy, cursors, actions and validation stay in Rust.
- `src/api/notifications.rs` and `src/protocol/notification_wire.rs`: preserve public contracts unless a concrete blocker requires a separately identified change.
- `src/activity/agenda_store/`: reference its existing Turso owner-thread, syscall I/O and shutdown patterns. Do not couple the notification store to the agenda schema or refactor agenda as part of this work.

There are no QML files in this repository. The external QML consumer should need no storage-related changes and must not open the database.

## Implementation sequence

### 1. Start with a fresh database

- Change the default in `src/activity/config.rs` from `notifications.sqlite3` to `notifications-turso-v1.db`.
- Keep `BAR_DAEMON_NOTIFICATION_DATABASE` as an exact path override. Existing installations using it must choose a fresh path for this cutover.
- Ignore the old default file and its sidecars. No automatic deletion of arbitrary configured files and no attempt to open legacy storage as the new store.
- Initialize only a newly created store. Give the new schema a notification-specific application ID and version. Reject existing incompatible, empty/truncated or corrupt files with a clear error directing the operator to a fresh path; never silently reset a new Turso store after a failure.
- Use private file permissions and one owner lock. Existing notification data is expendable, but accidentally modifying another domain's database is not.

**Done:** startup uses a clean store; new Turso history survives subsequent restarts; old SQLite storage is not consulted.

### 2. Replace the storage implementation, not the notification engine

- Retain the typed command enum and queue interface. Keep storage private to its named worker thread.
- Create and retain `turso::Database` and `turso::Connection` on that thread with `IoBackend::Syscall`. Drive async database operations there, following the existing agenda pattern; never block a Tokio executor thread with database work.
- Make startup an async ready/failed handshake and remove the now-redundant initialization `spawn_blocking` in `NotificationEngine::persistent`.
- Port parameter binding, optional-row handling, affected-row counts and row decoding explicitly. Check integer conversions instead of unchecked casts for IDs, timestamps and limits.
- Use explicit, awaited transaction completion/rollback. Preserve atomic settings-plus-prune, once-key-plus-save and DND-pair updates. Save payload and its derived fields atomically.
- Keep normal mutation admission asynchronous: accepting a queued notification is not a claim that it has already committed. Do not accidentally turn every popup into a synchronous disk round trip.
- Preserve the sticky failed-write fence for revisioned reads. Continue later accepted mutations after recoverable statement errors, but stop using a connection with an uncertain commit/failed rollback; do not expose uncommitted rows as history.
- Add an explicit drain/shutdown acknowledgement so tests and daemon teardown can close the worker before reopening storage. Checkpoint on clean shutdown, release ownership before replying, and report failures. Wire this into the actual daemon shutdown path, not just tests.

**Done:** no `rusqlite` types remain in notification production code and existing queue/cancellation semantics still hold.

### 3. Move the remaining SQL application logic into Rust

Use a small new schema with the existing conceptual tables: notification records, metadata, and once-only event keys. No compatibility columns or invalidation triggers are needed.

- Keep full `ActiveNotification` JSON as the authoritative payload.
- Compute normalized search text, stable app identity and a bounded persisted preview in Rust when saving. Store the preview separately so listing 5,000 candidates does not deserialize 5,000 full bodies/action lists.
- Share preview construction with the existing Rust projection code. Preserve Unicode clipping, captured-icon fallback, action-free repeat grouping and the current length boundaries. Do not serialize query-specific `matches` as durable state.
- Replace `json_extract`, `json_array_length` and SQL substring-based preview construction with those typed Rust fields. Replace the search-invalidation trigger with atomic payload/projection updates.
- Read at most the existing 5,000-record candidate window for history/center search; apply literal substring matching, cursor checks, active-record exclusion, ordering and selection in Rust. Fetch full payloads only for the requested page or selected detail, enforcing the existing 100-record/512-KiB page bounds.
- Keep simple SQL for indexed storage lookup, ordered candidate selection and deletes. Compute retention cutoffs in Rust; keep bulk age/count deletion in the database rather than materializing all retained payloads.
- For delete-by-app, use the stored Rust-derived app key across all retained rows, not only the searchable window. Preserve the 100,000-target safety limit.
- Adapt `center::project`'s synchronous selected-detail callback by prefetching at most the requested selected record in the worker, then passing a pure lookup. Do not push async storage into projection code or QML.

Keep current behavior: transient notifications are not retained, restarting does not revive old D-Bus conversations/actions/popups, age pruning can remove disk copies of live conversations without closing the live notification, and count retention protects still-live rows.

**Done:** no notification JSON SQL functions, business-rule triggers or materialized search CTE are required; no full-payload scan on every center refresh.

### 4. Qualify only the Turso features actually used

Add focused Rust tests against exactly 0.8.3, using temporary stores:

- New schema creation/reopen, primary-key generation without reuse after pruning, indexes, bound parameters, affected-row counts, conflict handling, commit and rollback.
- Verify the chosen journal/synchronous settings. Start with the agenda store's `synchronous=FULL` and measure write latency rather than assuming SQLite PRAGMA behavior carries over.
- Do not blindly copy `secure_delete=ON` or claim secure erasure. Verify support if retained; WAL/filesystem copies are outside such a guarantee.
- Map Turso `Busy`/`BusySnapshot` into bounded read recovery. Preserve the existing short read retry budget and cancellation checks; do not retry uncertain writes automatically.
- Replace the SQLite-specific DELETE-journal contention test with a real supported Turso contention case. Use Rust fault injection for deterministic failure-fence tests instead of adding test-only SQL triggers to production design.

No broad SQLite compatibility project is required. If an old SQL construct is inconvenient, simplify it rather than building an adapter.

### 5. Port tests, remove notification dependency usage, ship

- Update `persistence.rs` tests, `history_tests.rs`, `redesign_tests.rs` and engine restart fixtures to use Turso and explicit shutdown before reopening. Some current tests open a second engine while the first still owns the store; remove that assumption.
- Cover replace/close/clear, transient replacement, DND/settings/policies, once-only delivery, retention, Unicode/literal search, pagination, stale cursors, selected details, grouping boundaries and deletion beyond the candidate window.
- Keep queue saturation, cancelled reservations, accepted-write cancellation, failed-write fencing and settings/prune rollback coverage.
- Add restart/crash coverage for committed versus uncommitted writes and a clean-start test proving legacy default storage is ignored. Do not add legacy-data preservation tests.
- Compare current and new projection outputs on Rust fixtures, especially long Unicode bodies and repeat grouping. Measure center refresh and queue-drain latency at the 5,000-record window; catch accidental full-payload decoding.
- Remove `rusqlite` from normal dependencies once notification production code is converted. `src/agenda_maintenance.rs` currently uses it in tests: move it to dev-dependencies if still needed there. Repository-wide removal is not a prerequisite for this notification cutover.
- Update `Cargo.lock` only as needed, plus notification storage documentation and the changelog. Document the fresh-path requirement for overrides and intentional loss of old notification state.

## Validation and release gate

Run the notification unit tests, API/protocol contract tests, affected update-notification tests and the full Rust suite. Start with:

```text
cargo test --locked --lib activity::notifications
cargo test --locked
cargo check --locked --all-targets
```

Ship when fresh startup, normal use, explicit shutdown/restart and failure fencing work with Turso 0.8.3; notification production code no longer uses rusqlite; and application rules remain in Rust. No old-history preservation or QML rewrite is required.

## Deliberately excluded

Legacy migration/export/import, lossless downgrade, compatibility triggers, dual backend selection, cloud sync, a generic database abstraction, agenda refactoring, a new UI, and any tooling implemented in another language.
