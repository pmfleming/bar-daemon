# Five follow-up quality improvements

Baseline: `656ac64`. Evidence lives in `target/quality-next-five-656ac64/00..05`;
each directory has a derived `rqlens.toml`, RQLens reports, and validation logs.
The local analyzer executable is pinned by copying the previous review's binary.
Existing `src/activity/notifications/mod.rs` edits are left uncommitted.

Metrics below sum functions in the named files outside `::tests::` modules,
including new helpers and removed functions. Effort is rounded Halstead effort,
not elapsed time. Macro inventories remain partial; this is not certification.
No policy thresholds or exclusions are changed.

## 1. Update-history query and pagination

`src/updates/history.rs`: validation returns the bounded page size; query matching
and cursor parsing belong to `Query`. Pagination owns the byte budget and guarantees
progress for an oversized first row. A clock-injected projection enables an actual
cross-midnight cursor test. Hash input order, filtering precedence, wire errors,
retention anchors, and lazy search-text construction are unchanged.

- Cognitive sum: **38 → 29**; effort: **485,726 → 338,277**.
- Cyclomatic sum: **59 → 65**: helper bases and propagated errors increase the
  aggregate despite a simpler projection. This is reported rather than hidden.
- Added cursor offset/overflow, invalid-filter-with-run-ID, zero-limit, empty-page,
  and cross-midnight revision tests; existing oversized-page tests still pass.
- Targeted history tests, strict all-target Clippy, formatting, and the full private
  fixture suite pass (226 tests, zero ignored in the full run).

## 2. Authoritative stream descriptors

`protocol::read_wire` now owns stream order, snapshot keys, and typed projection
functions in one ordinary descriptor table. The public `STREAMS: &[&str]` remains
a compile-time constant derived from that table. API snapshot requests resolve
names once; state failure overlays iterate descriptors without repeated lookups.
The two parallel projection/key matches and their unreachable assertion are gone.
There is no new code-generation macro or full-snapshot serialization shortcut.

Across `protocol.rs`, `protocol/read_wire.rs`, `protocol/read_fixtures.rs`, `api.rs`,
and `state.rs`, non-test-module cognitive/cyclomatic/effort totals change from
**56 / 186 / 1,155,548 → 54 / 152 / 1,130,085**. The descriptor table is data, not
hidden branching; function metrics do not count its field-selection closures as
standalone functions. One test glob import is also removed.

Tests verify unique names/keys, complete snapshot-field coverage, typed projection
against serialized snapshots, unknown-stream behavior, and real committed events
versus resubscription. Formatting, strict all-target Clippy, and the full private
fixture suite pass (226 tests, zero ignored).
