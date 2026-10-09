# RQLens review and targeted refactor

Baseline: clean `bar-daemon` commit `220eb3d`. Used the local
`../rust-quality-lens/target/debug/rqlens`, frozen at
`target/refactor-review/rqlens` (SHA-256
`09b72768d645f97886ae4b22ced05553bbcac988fac290745e66f97405e94053`).
The analyzer checkout was at `b997a69`; this pass did not modify it.

## Findings and changes

- **Calendar projection:** nested membership decisions were the highest cognitive
  hotspot (21). Separate event membership from date enumeration, preserve
  half-open ends and zero-duration events, and use bounded date successors.
  Keep native dates through range projection instead of reparsing strings that
  were just formatted from those dates. This removes two redundant error paths.
- **Location projection:** the next cognitive hotspot (20) also cloned clocks and
  timezone keys into temporary lookup collections. Borrow lookup data, clone only
  output clocks, reuse string/vector allocations when applying weather overrides,
  and isolate weather-to-location conversion. Range checks already reject NaN and
  infinity; remove redundant finite checks. Identity ordering remains unchanged.
- **API effects:** eight call sites now share field/envelope/error construction.
  Preserve field names, error codes, chained anyhow errors, and plain audio errors.
  Remove the brightness-only future-forwarding wrapper. Domain mutations and
  publication order remain in their original owners.
- **State locality:** move the work-area subscription token into `StateStore`,
  which owns its counter. Remove the backwards `state -> work_area` dependency
  and the token's unnecessary public constructor. RAII cancellation remains intact.
- **Lid monitoring:** share successful completion handling and the eligibility
  predicate without changing retry ownership, inhibitor lifetime, or dispatch.
- **Test reuse:** replace 15 repeated persistent-notification setups with one
  test-only fixture. Every caller retains the temporary-directory guard; restart
  and database-failure assertions remain unchanged. Replace the calendar test's
  glob import with explicit imports.

## Comparable measurements

Same analyzer executable, configuration, thresholds, and 111-file source scope.
Totals include tests; function sums exclude module rows to avoid double counting.

| Metric | Before | After |
| --- | ---: | ---: |
| Cognitive sum / maximum | 2236 / 21 | 2216 / 18 |
| Cyclomatic sum / maximum | 4628 / 41 | 4617 / 41 |
| Sum of function Halstead effort | 19,801,130 | 19,699,789 |
| Sum of module Halstead effort | 136,029,516 | 135,730,140 |
| Calendar `project` cognitive / cyclomatic | 21 / 11 | 5 / 6 |
| Location `project` cognitive / cyclomatic | 20 / 17 | 8 / 10 |
| Duplicated lines / clone groups | 1602 / 127 | 1482 / 116 |
| Mean locality | 97.291 | 97.318 |
| `state` locality | 63.25 | 66.25 |
| Mean observed-reuse leverage | 26.757 | 26.667 |
| Escape-hatch occurrences | 29 | 28 |
| Explicit `.clone()` / `.cloned()` sites | 507 | 490 |
| Rust physical lines (`src` + `tests`) | 32784 | 32773 |
| RQLens nonblank source lines | 30511 | 30487 |

The extracted event and weather helpers have cognitive/cyclomatic values of
3/5 and 6/6 respectively; aggregate improvements above include them and the new
regression tests. Halstead effort is a syntax heuristic, not developer hours.
Clone-call counts include tests and exclude `clone_from`; they are not allocation
benchmarks. The escape-hatch reduction is one test glob import, not unsafe code.

**Leverage tradeoff:** real implementation reuse improved (eight response callers,
15 fixture callers), but RQLens's module leverage proxy counts inbound modules,
not call-site reuse. Removing the backwards state dependency lowers `work_area`'s
observed reuse from 30 to 20. Consequently the mean proxy slightly decreases;
this pass does not claim improvement in every aggregate metric. No artificial
consumers or module splits were added to inflate it. LOC reduction is also modest:
new boundary/error tests offset much of the removed scaffolding.

## Validation and evidence limits

- `rqlens verify`: **20 passed, 8 optional checks skipped**, no unavailable checks
  or compiler diagnostics. Includes formatting, check, strict Clippy, tests,
  rustdoc, audit, deny, and unused-dependency checks. Skips are not passes.
- **251 tests pass**, including private PipeWire and native-idle fixtures;
  **zero ignored** in the full-fixture run. No tests were removed.
- Changed-line coverage: **84% (135/160)**. Remaining gaps are effectful API paths
  and the live logind observer, plus two projection lines; this is not full
  integration coverage of the lid monitor.
- All three architecture rules report **zero violations**; collected references
  resolve completely (1286 before, 1285 after).
- Overall syntax-derived evidence remains **partial** because of pre-existing
  unexpanded macros and cross-module impl ownership. Complete reference resolution
  does not make the source inventory complete. Clone evidence is complete.
- No compiler-reported dead code existed at baseline or after this pass. Removed
  redundant paths/wrappers are not presented as a discovered unused subsystem.
- Reliability observations fall from 865 to 837, mostly through test-fixture
  deduplication. The genuine production diagnostics-serialization `expect` remains.
  RQLens also labels the existing test-only engine constructor and the new
  `#[cfg(test)]` fixture's unwraps as production; those are test scaffolding, not
  new runtime panic paths. No suppression was added.
- Concurrent edits in `../daemon-framework` briefly prevented compilation and
  later invalidated a repeat verification run's input fingerprint. This pass left
  that checkout untouched. Final verification used private, stable copies of both
  checkouts under `target/refactor-review/isolated/`: all 20 gates and 251 tests
  passed there too. Its authoritative `rust_practices.json` is under
  `isolated/bar-daemon/target/analysis/`. Static metrics and coverage above were
  collected in the original checkout; no controlled runtime benchmark or single
  dependency fingerprint across the entire review is claimed.

Raw reports and command logs are under `target/refactor-review/{before,after}/`;
coverage is in `target/refactor-review/fixtures/cobertura.xml` and
`target/refactor-review/diff-coverage.json`. The isolated verification log is
`target/refactor-review/isolated-verify.log`; the rejected live rerun is retained
as `target/refactor-review/after/verify.log`. Reproduce:

```sh
for metric in hotspots clones escape-hatches reliability locality leverage architecture-rules; do
  target/refactor-review/rqlens measure "$metric" --config rqlens.toml
done
target/refactor-review/rqlens verify --config rqlens.toml
bash tools/quality-fixtures.sh coverage # requires the documented dev-shell fixtures
```

## Remaining priorities

1. The live lid observer remains the largest production hotspot (score 198.02,
   cognitive 16, cyclomatic 28). Add a private logind/UPower observer fixture before
   attempting a larger ownership/retry refactor.
2. `ApiService::dispatch` still has maximum cyclomatic complexity 41. Its flat
   routing table is not equivalent to deeply nested policy logic; moving arms
   merely to lower that maximum would not necessarily reduce maintenance effort.
3. `daemon::tasks` and `api::effects` retain broad composition-root fan-out.
   Further reductions should follow domain ownership and integration coverage,
   not arbitrary file splitting or new generic frameworks.
