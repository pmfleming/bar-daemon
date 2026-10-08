# Notification-focused quality review

Baseline: clean bar-daemon `14d981b`. Tool: local
`../rust-quality-lens`, clean commit `b997a692242690cfb6bf7cab9f8296ec097497e8`.
Both runs used the same frozen executable, SHA-256
`09b72768d645f97886ae4b22ced05553bbcac988fac290745e66f97405e94053`,
and unchanged measurement settings. Raw evidence, logs, and temporary configs
are in `target/quality-current/`. Baseline/final fingerprints:
`590b7dd55a7e86dc` / `520d9badf9654f47`.

## Findings and changes

- **Identity scanning mixed traversal, bounded I/O, parsing, and indexing.**
  Separate those responsibilities while preserving XDG precedence, scan/depth
  budgets, file-size limits, and rejection of remote artwork. App-key rules and
  their assertions now live with identity; active notifications expose that rule
  without repeating four-field argument lists throughout the engine.
- **Center projection combined unrelated views and copied page entries.**
  Separate app aggregation from single-app selection. Only app-list requests
  build the grouping map; single-app requests filter in place. Move page entries
  instead of cloning them, retaining only the necessary overlapping overview
  copies. The selected-record callback is now explicitly `FnOnce`.
- **Persisted previews repeatedly allocated temporary field strings.** Borrow
  SQLite text while constructing previews. Live and saved repeat eligibility now
  share the same boundary checks; SQL clips at exclusion boundaries so truncated
  content cannot become a repeat key. Share catalog decoding and byte checks.
- **Timeline grouping used fallible JSON serialization for an internal key.**
  Use structural tuple equality, moving keys into stacks. Separate stacking from
  date/window projection. Calendar-month comparison no longer needs `expect`.
- **Repeated orchestration obscured contracts.** Share query normalization and
  engine catalog snapshots, use generated D-Bus signal emitters instead of a
  second handwritten signal schema, and keep notification routing/error mapping
  in `src/api/notifications.rs`. Ordinary `Result` propagation preserves existing
  error codes, envelopes, and validation-versus-backend precedence.
- **Redundant copies and generated code.** Upserts apply silence before their
  single retained-copy insertion. Remove the obsolete signal-interface constant
  and unused Clone/Deserialize/equality derives from the outbound history DTO.
  Share notification input fixtures locally with their model. No supported API,
  test, safety limit, dependency, or lint exception was removed to lower metrics.

## Comparable measurements

Same 108-file authored-Rust scope, including tests. Halstead effort is a syntax
heuristic, not measured development time. Aggregates count function rows only.

| Metric | Before | After |
| --- | ---: | ---: |
| Maximum cognitive complexity | 36 | 18 |
| Maximum cyclomatic complexity | 54 | 41 |
| Cognitive sum | 2,189 | 2,163 |
| Cyclomatic sum | 4,436 | 4,475 |
| Function Halstead effort sum | 18,919,008 | 18,591,192 |
| Detected duplicated lines / groups | 1,732 / 138 | 1,620 / 127 |
| Escape-hatch occurrences | 28 | 26 |
| Production-scoped panic findings | 4 | 2 |
| Mean locality | 97.333 | 97.333 |
| Mean observed-reuse leverage | 26.389 | 26.389 |
| Rust nonblank lines | 29,143 | 29,125 |
| Rust physical lines, `src` + `tests` | 31,288 | 31,281 |

The LOC reduction is deliberately modest: three regression tests were added,
existing tests extended, and no assertions discarded. Documentation is not part
of these Rust LOC totals.

Selected functions, cognitive / cyclomatic / Halstead effort:

| Function | Before | After |
| --- | ---: | ---: |
| Identity `collect` | 36 / 22 / 84,561 | 10 / 8 / 19,157 |
| Store `center` | 14 / 41 / 143,275 | 7 / 16 / 45,597 |
| Center `project` | 11 / 11 / 263,493 | 3 / 7 / 98,946 |
| Timeline `project_at` | 20 / 14 / 198,178 | 12 / 9 / 119,069 |
| API `dispatch` | 1 / 54 / 71,352 | 2 / 41 / 55,049 |

These functions delegate some work to explicit helpers; the aggregate effort
reduction includes those helpers. Aggregate cyclomatic complexity **increased**:
there are more small functions/tests, and notification decoding previously hidden
inside `request!` now exposes its `?` paths to the syntax extractor. The maximum
function effort remains the unchanged protocol fixture (394,315).

Shared rules and fixtures improve practical reuse without increasing the tool's
consumer-module counts. Persistence locality improves from 88 to 91 by removing
its timeline dependency; explicit API engine access offsets that gain. **There
is no aggregate leverage/locality improvement**, and no allocation or throughput
benchmark claim. App aggregation trades two bookkeeping maps for one map of
owned groups; repeat keys use owned tuple strings instead of encoded JSON.

## Validation and remaining limitations

- Full fixture coverage run: **225 tests passed, zero ignored** (223 unit tests,
  two integration tests), including private PipeWire and native Hypridle.
- Formatting, strict all-target Clippy, and `git diff --check` passed.
- `rqlens verify`: **20 passed, 8 optional checks skipped**, no unavailable checks
  or compiler diagnostics. Includes doctests, rustdoc and configured dependency
  checks. Skips are not passes; no MSRV toolchain run was added.
- Changed-line coverage: **86% (464/534)** using `diff-cover` against HEAD.
  The new notification API routing still has uncovered success/error branches;
  engine and private D-Bus tests exercise the underlying operations, but are not
  a substitute for complete transport-level coverage.
- All **1,220 collected dependency references resolve** (1,215 local, five
  external). No architecture-rule violation was observed.
- Nevertheless, **`rqlens check` fails the architecture policy's completeness
  gate**: pre-existing unexpanded state/Wayland macros and lexical impl ownership
  make inventory evidence partial. Focused measurement also leaves unrelated
  producer artifacts absent. Do not present either absence as a verified pass.
- Two remaining production-scoped panic advisories are unchanged: the test-only
  engine constructor is classified as production, and `src/lib.rs` expects
  diagnostics serialization to succeed. No lint suppression was added to hide them.
- Larger sleep/lid workflows and additional transport coverage remain follow-up
  work; this is not a project-wide elimination of complexity or dead code.

Reproduce with the local frozen binary:

```sh
for metric in hotspots clones escape-hatches reliability locality leverage architecture-rules; do
  target/quality-current/rqlens measure "$metric" --config rqlens.toml
done
target/quality-current/rqlens verify --config rqlens.toml
# Expected to fail while macro inventory is partial:
target/quality-current/rqlens check --config rqlens.toml
cargo clippy --workspace --all-targets --locked -- -D warnings
QUALITY_OUTPUT_DIR=target/quality-review-coverage bash tools/quality-fixtures.sh coverage
diff-cover target/quality-review-coverage/cobertura.xml --compare-branch HEAD --fail-under 80
```
