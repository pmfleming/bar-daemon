# Five follow-up quality improvements

Baseline: `656ac64`. Evidence lives in `target/quality-next-five-656ac64/00..05`;
each directory has a derived `rqlens.toml`, RQLens reports, and validation logs.
The local analyzer executable is pinned by copying the previous review's binary.
Existing `src/activity/notifications/mod.rs` edits are left uncommitted.

Per-step metrics sum functions in the named files outside test modules,
including new helpers and removed functions. The combined comparison also excludes
external `_tests` modules from its outside-test-module subtotal. Effort is rounded Halstead effort,
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

## 3. Watcher policy, installation, and recovery

`updates/watch.rs`: event relevance and topology rules live in `EventPolicy`;
installation owns the complete bounded watch plan and registration. `refresh_at`
only reconciles target identity and backoff. Planning/registration failures share
one recovery path (and warning), and partial installation is dropped on failure.
The registration loop and callback share an immutable path slice rather than
cloning every `PathBuf`. Fixed-window coalescing is unchanged.

- Whole production file cognitive/effort: **26 / 180,341 → 23 / 91,893**.
- Cyclomatic total: **39 → 44** (four additional function bases plus propagation).
  `refresh_at` itself changes **17 / 23 → 8 / 9** cognitive/cyclomatic.
- Removed a test glob import. Added direct tests for ledger/log relevance,
  ancestor events, removed/new-directory renames, read/write-close distinction,
  and rescan/errors behind a full invalidation queue.
- Targeted watcher tests, strict all-target Clippy, formatting, and full private
  fixtures pass (228 tests, zero ignored in the full run), including real inode
  replacement, watch-budget exhaustion, symlink exclusion, and recovery backoff.

## 4. Deployment evidence and catalog recovery

`updates/manual.rs` classifies deployment evidence once as unrecorded, first
recorded, changed, or unchanged. The phase and completed outcome derive from that
classification; execution failure/cancellation remains an independent fact. Thus
partial failure can retain activation, but a claimed deployment without an after
system cannot invent an activation or a known version change.

`updates/catalog.rs` indexes borrowed `(lane, scope, name)` tuples instead of
formatting owned recovery keys. Annotation vectors are temporarily detached so
catalog identity can stay immutably borrowed; their capacity is retained. Recovery
is owned by `RunIssue`, explicitly requiring a strictly later timestamp. The final
annotations and open-issue wire representation remain unchanged.

Across these two production files: cognitive/effort **65 / 512,656 → 56 / 486,723**;
cyclomatic **103 → 109**, including the new classification/recovery boundaries.
This adds explicit types/helpers rather than claiming a reduction in every signal.
Tests extend deployment/failure combinations, untrusted system identities, lane and
scope isolation, and repeated annotation. Existing cancellation, equal-time,
retention, partial-publication, and recovery tests pass. Targeted update tests,
formatting, strict all-target Clippy, and full fixtures pass (230 tests, zero
ignored in the full run).

## 5. Explicit timezone observation lifecycle

`timezone.rs` shares a `Cached<T>` lifecycle between timezone and accepted-location
reads: pending admission, retained verified value, last error, bounded retry, and
success reset. Idle read futures are never polled. `Observations` owns projection
and status-revision invalidation; the monitor retains subscribe-before-read ordering,
sequential reads, owner-change handling, and local time ticks. A failed timezone
read clears availability without discarding cached location; location-reader errors
still do not override public status or the timezone error.

Production cognitive/cyclomatic/effort: **40 / 77 / 267,733 → 31 / 79 / 224,428**.
`monitor_connection` changes **16 / 17 → 3 / 10** cognitive/cyclomatic. Paused-clock
tests exercise idle-future laziness, exponential retry capping/reset, retained values,
and projection/invalidation distinctions. The private D-Bus test continues to check
idle service behavior, revision changes, recovery, owner changes, and torn reads.

The final architecture run also prompted a small import follow-up in watcher tests:
import `DEFAULT_WATCH_LIMIT` directly from its defining dependency instead of via a
parent import. After forcing a fresh semantic cache, every collected reference
resolves. No rule is waived or disabled to accomplish this.

Targeted timezone tests, formatting, strict all-target Clippy, doctests, cargo-shear,
`git diff --check`, and full private fixtures pass. The final fixture run executes
**232 tests with zero ignored** (200 library, 30 location-daemon, two integration).

## Combined evidence and trade-offs

Across all 13 changed Rust files, baseline `00` versus final `05`:

| Signal | Before | After |
| --- | ---: | ---: |
| Function cognitive sum, including tests | 259 | 236 |
| Function cyclomatic sum, including tests | 541 | 541 |
| Function Halstead effort, including tests | 3,514,872 | 3,417,292 |
| Outside-test-module cognitive sum | 225 | 193 |
| Outside-test-module cyclomatic sum | 464 | 449 |
| Outside-test-module Halstead effort | 2,602,003 | 2,271,407 |
| Whole-file Halstead effort, including tests/data | 15,436,994 | 18,094,363 |
| Project duplicated lines | 1,602 | 1,599 |
| Project escape-hatch occurrences | 39 | 36 |
| Literal `.clone()` calls, `src/` + `tests/` | 652 | 652 |
| Physical Rust lines, `src/` + `tests/` | 42,945 | 43,365 |

The explicit state/types and broader regression coverage add **420 Rust lines**;
this pass does not achieve an overall LOC reduction. Whole-file effort also rises:
Halstead is nonlinear and file totals include table declarations and added tests,
unlike per-function totals. Helpers and tests are not omitted from their respective
aggregate measurements. Raw clone-call counts are unchanged, although production
recovery-key string allocations and deep watcher path-list copies are removed.

Architecture evidence uses frozen before/after project sources against identical
frozen local dependencies in `architecture/{before,after}`. All three configured
rules have zero observed violations and zero unresolved collected references;
macro/impl inventories remain partial. `read_wire` reach rises **11 → 12** and the
new generic cache is reused by both readers. Observed timezone/watch leverage scores
rise **20 → 30** and **10 → 20**, but these gains include explicit test dependencies,
not new production consumers. Protocol locality risk rises **52.5 → 55.5** with the
new authoritative descriptor dependency; state risk rises **48 → 48.75**. Therefore
no across-the-board module-locality improvement is claimed. Correct ownership and
within-module reuse, rather than graph-score manipulation, motivate these changes.
