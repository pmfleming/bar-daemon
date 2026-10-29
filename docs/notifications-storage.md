# Native notification storage and cutover

## Deployment

Native notifications now use embedded **Turso 0.8.3**, pinned with default features disabled. No server, network database, migration script or QML storage component is needed.

1. Stop the old daemon normally before starting the rebuilt daemon.
2. With the default configuration, no file preparation is needed: the new store is `$XDG_STATE_HOME/bar-daemon/notifications-turso-v1.db` (normally `~/.local/state/bar-daemon/notifications-turso-v1.db`). The old `notifications.sqlite3` and its sidecars are ignored and not deleted.
3. If `BAR_DAEMON_NOTIFICATION_DATABASE` is set, change it to a **nonexistent file** in a dedicated user-owned state directory. The override is still an exact path, not a directory. Do not pre-create an empty database or point it at the old SQLite file. The containing directory is made private (0700); database, lock and WAL files are 0600.
4. Rebuild/deploy and start the daemon. Startup failures include the database path and underlying cause; an ownership error means another owner must stop, not that the database should be reset.
5. Verify notification delivery, center search, DND/settings and a normal restart. Restarted stored records are closed history, never revived actions or popups.

**Intentional storage break:** old notifications, DND, settings, per-app policies, notification ID high-water marks and once-only delivery keys are not imported. Fresh-store defaults apply. Resetting once-only keys may allow a producer to deliver an old event again; the independent update-delivery ledger is not reset. There is no dual writer or lossless downgrade. Do not point an older binary at the Turso file.

Existing new-format stores are retained across restarts. A wrong application ID/schema, corrupt store or preexisting empty file is an error, not permission to silently replace it. A reset is an explicit operator decision: stop the daemon and select another fresh path. Never remove/copy only the main file of a live database; committed data can be in its WAL. This cutover does not promise secure erasure of old files, WAL, snapshots or backups.

## Ownership and Rust/QML boundary

- One named `notification-history` thread owns the database, connection and file lock. Turso uses syscall I/O and `synchronous=FULL`.
- A 1,024-command channel preserves the engine's reservation-before-mutation and ordered enqueue semantics. Cancelling an unaccepted reservation has no effects. Dropping a caller after acceptance does not undo its writes, including once-only delivery.
- Ordinary notifications acknowledge in-memory admission, not disk commit. Settings, policies, deletion and once-only operations return their storage results. There is no automatic replay of an uncertain write.
- Save payload/search/preview/app identity, once-key/save/prune, settings/prune and DND-pair updates use explicit transactions. Recoverable statement errors roll back. A failed queued mutation permanently fences revisioned catalog reads for that worker; later safe accepted mutations can still persist. An uncertain commit or failed rollback fences the entire connection until restart, including unrevisioned reads.
- Daemon shutdown stops producers, closes admission, drains accepted commands and outstanding reservations, checks WAL checkpoint completion, and releases ownership before acknowledging. Dropping all handles also drains, but is asynchronous; restart tests use explicit shutdown.
- Rust computes identity, normalized literal search, previews, cursor checks, sorting, grouping, retention cutoffs, deletion targets and selected-record hydration. SQL only stores and retrieves indexed data. There are no JSON SQL functions, business-rule triggers, compatibility backfills or cloud sync.
- Center/history search examines at most 5,000 persisted previews plus the bounded active overlay. Empty searches do not load normalized bodies. Only requested details/page payloads are deserialized; history retains its 100-record/512-KiB page bounds. Deletion covers all retained rows, not only the search window.
- QML continues to render daemon-produced views and send commands. No wire/API changes or new client fallback are needed. No new implementation/test languages were added.

`rusqlite` remains only as a dev-dependency for unrelated agenda maintenance fixtures; it is absent from the normal notification/daemon dependency graph.

## Qualification

All qualification uses temporary stores, never live user data:

```text
cargo test --locked --lib activity::notifications
cargo test --locked
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --lib activity::notifications -- --include-ignored --test-threads=1 --nocapture
cargo tree --locked -e normal -i rusqlite
```

The explicitly ignored large-window test is included in the release command. It seeds 5,005 records with 16-KiB bodies, checks the 5,000-candidate window, live-row retention, app deletion beyond that window, high-water identity preservation and non-reuse after deletion. Other tests cover projection parity/Unicode/repeat boundaries, literal search, byte bounds, lifecycle/restarts, queue cancellation, transaction rollback, once-only keys, competing owners, incompatible stores, private files, actual Turso contention and timeout restoration, commit/rollback fault fencing, and abrupt subprocess exit with committed plus uncommitted WAL state. The crash test is not a claim of physical power-loss or device-failure testing.

Completion results on this checkout:

- Full `cargo test --locked`: 325 tests passed across all targets; 8 environment/performance-specific tests ignored. The large notification-window test was additionally run explicitly below.
- Release notification suite, including the large-window test: **44 passed**, none ignored.
- All-target check and strict Clippy: passed without warnings.
- Normal dependency graph: no `rusqlite` dependency. `Cargo.lock` needed no change because Turso was already pinned and SQLite remains a test-only fixture dependency.
- Local release observations: 5,000-preview refresh **95–190 ms**, 100 FULL-sync writes drained in **0.54–1.63 s** across two runs. Debug refresh was about 1.4 s. These are measurements on this checkout, not latency guarantees.

No live store was opened or deployed as part of qualification. Source is ready for the normal rebuild/cutover; environment-specific compositor/native-idle probes still require their usual external services.
