# Agenda store

Pinned embedded `turso =0.8.0`, no default features; current lock resolves core/sdk 0.8.3, matching Scratchpad. Syscall I/O and `synchronous=FULL`; no Scratchpad database is opened. Rust >=1.90 is required by the locked engine dependencies.

## Ownership and durability

A private dedicated directory contains `agenda.db`, its WAL, `agenda.db.lock`, and any legacy backup. One named worker owns the connection and lock. Its 32-entry async channel acknowledges through oneshots only after COMMIT. Dropping an async waiter does not undo an accepted write: query its receipt. A missing reply is not permission to replay automatically. Receipts use SHA-256 over deterministic typed mutation serialization; different content under a used request ID is rejected. Receipts are never evicted; at 100,000 receipts new writes are refused until explicit maintenance/reconciliation. Items are capped at 10,000; committed item/receipt/migration payloads together are capped at 100 MiB (checked inside the mutation/import/restore transaction). This also bounds logical exports below the 512 MiB restore-input limit. Per-record bounded JSON BLOBs contain schedule/details/links/checklist atomically, with STRICT SQL identities, source FK, revision, kind and payload-size constraints. No JSON SQL extension is needed.

`shutdown().await` closes admission, drains accepted jobs, verifies checkpoint completion, releases the ownership lock and then replies. Last-handle drop also drains/checkpoints on the owner thread, but is asynchronous; use explicit shutdown before immediately reopening a test directory. Commit errors (potentially ambiguous) and rollback failures fence further writes and healthy-checkpoint acknowledgement until reopen/receipt reconciliation. If rollback failure leaves the connection inside its transaction, item/catalog/receipt reads also fail closed: that connection could otherwise report its own uncommitted data as durable. Committed reads remain available once autocommit is confirmed, so an already-durable ambiguous outcome can still be resolved by receipt. No speculative record cache exists inside storage.

## Legacy adoption

The caller supplies the actual `activity::config::todo_path()` (`BAR_DAEMON_TODO_FILE` or `$XDG_STATE_HOME/bar-daemon/todos.json`, normally `~/.local/state/bar-daemon/todos.json`; existing path-helper fallback is retained). The service uses its sibling `agenda/agenda.db`, while `BAR_DAEMON_ACTIVITY_CONFIG`/`$XDG_CONFIG_HOME/bar-daemon/activity.json` stays untouched. The actual file is a JSON array of `TodoItem`, not a wrapper. The service must stop JSON writes before opening the store. Missing and unreadable/corrupt files are distinct. Invalid/duplicate records block writable startup without dropping original bytes.

IDs, source IDs, title, u8 numeric priority (including historical values >9), creation and completion timestamps/state are retained. A civil due date becomes an end-only inclusive date schedule. An instant due value becomes an end-only timed schedule with UTC original zone (legacy has no stored zone). No inferred start is persisted. Historical records containing both due representations use the explicit civil date for the active schedule; both exact original fields remain in the private original backup and migration receipt. Historical contradictory open/completion-timestamp or malformed schedules block migration rather than being normalized silently.

The original bytes are fsynced to a content-addressed private backup before the one-transaction import/provenance receipt. The database receipt prevents a second import; changed legacy bytes require explicit reconciliation. A removed legacy file after adoption is allowed. Calendar configuration is not moved or rewritten.

## Export and restore / rollback

Never copy just a live `agenda.db`: committed data may be in its WAL. The supported backup is a logical owner-serialized export containing current records (including post-migration edits), sources, catalog revision, operation receipts and original migration provenance.

1. Stop the daemon normally. Keep the original database directory and legacy/configuration files intact.
2. Run `bar-daemon agenda export --store-dir /absolute/agenda-directory --output /absolute/new-export.json`. The guarded offline maintenance CLI calls `Store::open_existing` and `Store::export`. It takes the same ownership lock; export destination must not exist. Output is private and fsynced. `open_existing` opens an existing held file without O_CREAT, checks inode/device after engine open, refuses absent/empty/unknown schema, and uses the engine's read-only mode. This prevents a missing-source export from creating a successful empty store. Read-only shutdown closes without writing/checkpointing; committed WAL is read by the engine. `Store::open` itself intentionally creates missing stores for normal daemon startup and must not be used as an unguarded export CLI.
3. Run `bar-daemon agenda restore --store-dir /absolute/new-restore-directory --input /absolute/new-export.json`. The CLI requires a nonexistent destination directory and uses `Store::restore`; never overwrite the source directory.
4. Restoration validates application/schema identity and model/source/receipt/provenance invariants in one transaction. Open/check the restored directory with the same release before switching the daemon's store root. Keep the old directory for recovery. `agenda-turso-contract` accepts no paths and is only an isolated temporary-directory smoke test.

Restoring only old `todos.json` loses post-migration edits and is **not** a supported lossless rollback. Unknown/newer database or export schemas refuse writes; downgrade never clears a database. Schema initialization is permitted only for a database file exclusively newly created by this owner, never a preexisting empty/truncated/zero-schema file. An existing zero-byte main file with a WAL is still passed to engine recovery, because committed identity/data may reside in WAL. Interrupted initial schema creation that leaves unknown storage fails closed for explicit recovery, not automatic empty replacement. Exports are versioned schema 1, plaintext private data, not encrypted backups.

## Qualification

`cargo test --locked --lib activity::agenda_store::tests -- --nocapture`

`cargo test --locked --release --lib activity::agenda_store::tests -- --nocapture`

`cargo run --locked --bin agenda-turso-contract` uses only temporary isolated directories.

Tests exercise real Turso STRICT/CHECK/FK/prepared writes, a 16 MiB BLOB commit/rollback/checkpoint contract, existing-only refusal/read-only mutation guards, durable receipts, completion/checklist independence, revision races, cancelled async waiters, legacy corruption/duplicates/partial-import rollback/retry, actual subprocess termination after the first imported row before migration receipt/COMMIT and duplicate-free restart, permissions, competing owner locks, unknown schema, corrupt database, abrupt subprocess exit with committed/uncommitted WAL, consistent export/restore and invalid-restore rollback. The VFS write-failure test applies an isolated subprocess RLIMIT_FSIZE, yielding real EFBIG; this is not a claim of physical-device ENOSPC/power-loss testing. Rollback-failure fencing is tested by injecting an actual engine rollback error into the production recovery policy. No live user store is used.
