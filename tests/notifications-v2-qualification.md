# Notification v2 release qualification

Qualified on 2026-11-02 with Rust 1.95.0, embedded Turso 0.8.3 and Qt 6.11.1.
Implementation commits: bar-daemon `32b8558` and `5515768`; paired Shelllist
`29054ad`. Later completion commits update documentation only.

## Results

- `cargo test --locked`: **342 passed, 0 failed, 8 ignored** across all targets.
  The large notification-window test is one of those ignored cases and was
  explicitly run below; unrelated environment/performance probes remain skipped.
- `cargo test --locked --release --lib activity::notifications -- --include-ignored
  --test-threads=1 --nocapture`: **49 passed**, no failures or skips.
- `cargo check --locked --all-targets`, strict all-target Clippy (`-D warnings`),
  `cargo fmt --all -- --check` and the debug daemon build: passed.
- Shelllist's full actual Qt suite: **714 passed, 0 failed, 1 existing skip**.
  Strict QML lint passed. No new skip or test language was introduced.
- `cargo xtask contract bar --bin ../bar-daemon/target/debug/bar-daemon` in
  Shelllist: the built producer's checked fixtures and frontend registry match.
- `cargo xtask build-inputs` in Shelllist: aligned Rust 1.95.0, 12 locked roots,
  49 shared dependencies. Existing sibling-source resolution remains intact;
  no new local revision pin or persistent lock entry was introduced.

## Storage witnesses

Tests cover typed scalar/hint/timestamp round-trips, ordered sender/context
actions, captured identity, non-restoration of runtime popup state, settings,
policies, DND and ID high-water persistence. Replacement preserves history ID
and removes obsolete children. Injected failure after child removal rolls back
both parent content and children. Transient replacement, pruning and bulk
deletion remove all dependent rows; once-only keys survive history deletion.

SQL substring matching is compared with the Rust reference for Unicode casing,
embedded NUL, quotes, `%`, `_` and missing matches. Cursor order, tied timestamps,
active exclusions and the insertion-order scope are retained. A complete
200-record active overlay and multi-batch deletion exercise planner bounds.
Selected details hydrate separately from previews and byte-bounded pages progress.

Two subprocess starts prove the default ignores both SQLite and Turso v1 files
and their sidecars, starts empty, then reopens v2 with history/settings/DND intact.
Existing v1-version overrides are rejected without changing their main-file
bytes. The abrupt-exit test preserves committed WAL state and discards an
uncommitted update; this does not simulate physical power loss.

The new QML regression exercises immediate cache/live-state release, stale
deletion-token retirement, retained unsent reply text, ignored late reads, lower
revision acceptance, empty history and delivery after cutover. It uses the actual
QML state/backend/catalog/timeline components with recording transport.

## Performance observations

The pre-change release run passed all 44 then-existing notification tests. It
measured a 5,000-preview refresh at **82.37 ms** and 100 FULL-sync commands at
**1.40 s**. After implementation, a separate release run without other test
suites measured the 5,000-preview refresh at **86.68 ms**, a byte-bounded SQL
history page at **81.45 ms**, and 100 FULL-sync commands at **2.60 s**. The full
v2 release run measured the latter at **1.92 s**. These are local observations,
not latency guarantees; durable writes now update several typed tables. No
write-throughput improvement is claimed. Notification admission remains queued
and asynchronous. Fixture seed times are not compared: v1 used a prepared
fixture insert while v2 exercises the real multi-table save path.

During development a long SQL OR predicate for live exclusions overflowed the
pinned engine's debug planner stack. The final implementation fetches at most
one extra candidate identity per live record and excludes the live overlay in
Rust. Delete predicates use batches of 16 to bound expression depth. The final
debug and release suites pass with the complete overlay; no worker-stack-size
increase or engine upgrade is used to conceal the failure.

## Deployment boundary

All storage tests used temporary directories. No live store was read, deleted,
migrated or deployed. The source cutover takes effect when the matched local
build is deployed and services restart (or the system reboots). The default v2
path needs no preparation; an explicit database override must name a fresh
file. Old history, DND/settings, policies, ID counters and once-only keys reset;
old producer events may be delivered again. See
[deployment instructions](../docs/notifications-storage.md).
