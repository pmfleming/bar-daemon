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
