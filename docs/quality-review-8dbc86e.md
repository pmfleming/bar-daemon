# Update catalog quality review

Baseline: `8dbc86e`. Analysis used the local `../rust-quality-lens` checkout
(`7461a919f6b93cefc3066feb9a3fb9336ca30396`, clean), with one copied executable
for both measurements. Binary SHA-256:
`f9b585a29d0cee1f0d3ffb39ae4390b09636ba5c39e9bd922540b5ca103b9113`.
No measurement settings, policy limits, or fixture selections were relaxed.

## Findings and changes

The three largest production cognitive hotspots were update notification delivery,
retained-issue recovery, and manual-report conversion. Shared catalog types lived
in the file reader, producing reader/recovery and reader/manual-reader dependency
cycles. Recovery sorting cloned run IDs on comparisons; recovery bookkeeping also
cloned keys and IDs unnecessarily. Worker history was sorted before being merged
and sorted again. Notification tests repeated the same publication/scan setup.

- `updates/catalog.rs` replaces `issues.rs`, owning catalog data, outcome predicates,
  and display-only recovery together. It has **no outbound internal dependencies**.
  Readers and delivery borrow its types rather than depending on the history reader.
- Delivery separates synchronous event selection from notification transport and
  cursor persistence. Persistence still follows successful notification delivery;
  silent initial imports, catch-up summaries, incident suppression, and restart
  idempotency retain their existing tests.
- Recovery classification is separate from chronological annotation. Sorting borrows
  IDs; pending equal-time failures remain pending without removing/reinserting keys.
  Notification incidents and display recovery deliberately retain different hold
  semantics; they were not combined into an incorrect shared policy.
- Manual-report validation is separate from projection. Status matching and
  deployment predicates are simpler; validated log-path normalization is preserved.
  A new table-driven test covers unknown, unchanged, and changed system identities.
- Overview normalization borrows JSON and validates the tools array without a
  subsequent `unwrap`. Scoped producer threads borrow candidates. Duplicate fixture
  construction and scans are shared, and history is sorted only after source merging.
- Removed four unused `Clone` derives and two unused `Debug` derives; made the raw
  worker-history loader private. Removed six test glob imports, without new lint
  suppressions. No compiler-confirmed dead-function inventory was available, so this
  is not a claim that all dead code has been eliminated.

## Measurements

RQLens function metrics (cognitive / cyclomatic / Halstead effort):

| Function | Before | After |
| --- | ---: | ---: |
| `Publisher::reconcile` | 41 / 39 / 258,680 | 4 / 14 / 35,521 |
| `annotate` | 39 / 23 / 265,611 | 17 / 9 / 142,417 |
| manual `entry` | 27 / 35 / 261,129 | 17 / 24 / 117,499 |

Extracted functions still perform the work. To avoid presenting extraction alone as
an aggregate improvement, these totals include **all functions under `src/updates/`,
including tests and helpers**, on both sides:

| Measure | Before | After |
| --- | ---: | ---: |
| Function count | 94 | 103 |
| Cognitive sum | 308 | 277 |
| Cyclomatic sum | 409 | 416 |
| Function Halstead-effort sum | 3,169,229 | 2,806,613 |
| Function SLOC sum | 2,712 | 2,677 |
| Repository Rust physical lines, including tests | 40,656 | 40,651 |
| Repository `.clone()` call sites | 624 | 616 |
| Repository RQLens clone groups | 119 | 114 |
| Repository escape-hatch observations | 42 | 36 |
| Repository reliability findings | 1,037 | 1,030 |

**Trade-offs:** aggregate cyclomatic complexity increases by seven, despite smaller
individual hotspots. Total physical-line reduction is only five after retaining
regression coverage. Escape-hatch reduction is entirely explicit test imports, not
removal of unsafe code. Reliability counts include test panic paths, not confirmed
production defects. Clone groups are duplicated-code findings, not runtime copies.

Catalog leverage is **30**, versus the former issues module's **10** (three observed
consumers rather than one). History leverage drops **70 → 50**, intentionally moving
reuse from an I/O module to the shared catalog. Module locality scores are unchanged:
`updates`/`desktop` 88, `history` 97, other update modules 100. The locality improvement
is structural—removing dependency cycles and keeping data/recovery together—not a
claim of increased aggregate locality or leverage scores.

## Verification and evidence limits

Both baseline and final full-fixture runs passed **191 tests, zero ignored**, including
private PipeWire and native-idle fixtures. Final checks also passed:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --doc --locked` (zero doctests)
- `git diff --check`

The outcome-code search assertion was moved into the existing history-query table;
one deployment table test was added, so the total test count is unchanged.

Local artifacts are in `target/quality-refactor-current/`: `before/`, `after/`,
`before.toml`, `after.toml`, `tests-before/`, `tests-validated/`, `clippy-final.log`,
and `check.log`. The derived configs change only project/output paths. Producers:

```sh
for tool in hotspots clones escape-hatches reliability locality leverage architecture-rules; do
  target/quality-refactor-current/rqlens measure "$tool" \
    --config target/quality-refactor-current/after.toml
done
```

Architecture reports zero violations of the three configured rules and all collected
references resolved (1,791 before; 1,808 after). **Evidence remains partial** because
of unexpanded macros and unresolved lexical ownership in the syntax inventory.
`rqlens check --fail-on partial` fails, as expected; this focused run also omits full
coverage/practices/type-health producers. No complete RQLens certification, coverage
improvement, or performance benchmark result is claimed.

## Remaining priorities

- Worker-history `load` (cognitive 23), manual `load` (19), and update monitor
  orchestration (21) remain worthwhile next targets, especially bounded error
  collection and cancellation behavior.
- Composition-root fan-out remains high (`daemon::tasks`: 23; `api`: 19 under this
  analyzer). Reconcile historical documented baselines with current identity
  extraction before judging regressions; do not introduce forwarding facades.
- Complete compiler/macro inventory and fresh full-fixture coverage are still needed
  for strict certification. No missing evidence was suppressed in this pass.
