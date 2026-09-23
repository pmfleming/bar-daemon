# RQLens review: domain simplification

Baseline: clean `c6e6aeb98bcaa17772a89f0d8578d8582f0f0618`.
Tool: `../rust-quality-lens/target/debug/rqlens` 0.1.0, complexity model v2,
risk model v4. The initial domain refactor retained configuration, source inventory
(81 files), policy thresholds, and existing tests. The table records that review,
not a re-baseline with subsequent analyzer/helper fixes. Historical evidence is in
ignored `target/review-before/` and `target/review-after/`; follow-up measurements
are in `target/review-follow-up/`.

## Measurements

Totals include Rust tests; no source files were moved out of measurement scope.

| Metric | Before | After |
| --- | ---: | ---: |
| Cognitive complexity, sum / maximum | 1614 / 23 | 1594 / 18 |
| Cyclomatic complexity, sum / maximum | 3315 / 48 | 3304 / 48 |
| Maximum function hotspot score | 189.00 | 161.95 |
| Duplicated lines / clone groups | 1252 / 95 | 1238 / 94 |
| Escape-hatch occurrences | 26 | 25 |
| Explicit `.clone()` / `.cloned()` calls | 278 | 276 |
| Mean module leverage | 66.062 | 66.062 |
| Mean module locality | 97.556 | 97.593 |
| Internal dependency edges | 241 | 241 |
| Physical Rust lines, `src` + `tests` | 22188 | 22179 |
| RQLens nonblank source lines | 20567 | 20554 |

The aggregate gains and line reduction are modest. RQLens does **not** provide
Halstead effort or measured developer effort; hotspot scores are heuristic
maintenance-pressure evidence, not an effort benchmark.

## Changes and trade-offs

- **ICS:** component-stack validation is separate from property parsing. The
  stack proves event ownership, eliminating redundant optional-builder checks.
  Parameters borrow input strings; the default end no longer clones an unused
  timezone. Parser cognitive complexity falls from 23 to 11; the whole module,
  including the new helper and strengthened tests, falls from 52 to 41.
- **Forecasting:** separate active-estimate calculation from presentation status.
  The former 20-cognitive/24-cyclomatic function becomes two functions with
  cognitive 10/10 and cyclomatic 17/7. This improves per-function comprehensibility,
  **not** aggregate forecasting complexity. Arithmetic and status precedence are
  preserved, including inconsistent input flags and bounded telemetry.
- **Persistence:** the notification worker owns its SQLite connection outright.
  Removed unnecessary `Arc`, `Mutex`, cloning, and the unreachable poisoned-lock
  error path. Mutation dispatch is shared without changing write ordering,
  reservation/cancellation behavior, transactions, or error continuation.
- **Activity:** configuration loading owns validation, eliminating duplicate
  validation and `unwrap_err`. Removed the always-false publication parameter;
  shared the syncing transition. Source IDs and cached forecasts are borrowed
  until ownership is actually needed. Media cycling reads only media state,
  rather than cloning every snapshot domain.
- **Locality:** world-clock construction belongs with `WorldClockState`, not the
  refresh service. Activity service leverage/locality improves **57/94 → 60/97**;
  the model's leverage falls **85 → 82**, with locality still 100. Average leverage
  is unchanged: this is responsibility placement, not fewer dependencies.
- **Battery API:** optional policy updates use defaults directly, preserving
  explicit false values, legacy-action precedence, validation, and wire behavior.
  The escape reduction is one test glob import, not an unsafe-code reduction.

Broader monitor/API orchestration edits were explored and reverted: they worsened
aggregate architecture scores and lacked changed-line coverage. No forwarding
facades, new lint suppressions, or policy exemptions were added.

## Verification and remaining work

- Formatting, strict all-target Clippy and doctests pass. **All 121 tests pass**
  with `--include-ignored`, including isolated PipeWire and native idle, both in
  coverage execution and the sandboxed Nix package's release-profile checks.
  No existing tests were deleted or skipped.
- Strengthened ICS tests cover adjacent/cancelled events, builder reset, default
  all-day duration, malformed nesting and properties outside a calendar. Existing
  Activity tests now verify default/custom world-clock labels and UTC metadata.
- A temporary differential harness compared serialized forecasts against the
  baseline across **401,408** flag/percentage/limit/time-bound combinations;
  all matched. Its source, baseline module and log are archived in
  `target/review-before/`, not added as duplicate production/test modules.
- The initial RQLens changed-line coverage was **97.11%**. Fresh full-fixture
  Cobertura execution passes the unchanged `diff-cover --fail-under 80` gate at
  **97%** (175/180 executable production lines). Scope definitions differ between
  tools; integration-test execution does not imply coverage of the C++ fixture.
- Architecture is now **complete**: **774/774** references resolve locally,
  zero unresolved/external references, three configured rules, zero violations,
  and a passing `rqlens check`. Existing composition-root baselines are unchanged.
- Remaining hotspots include managed lid monitoring (score 161.95), display-layout
  reconciliation, and API dispatch (cyclomatic 48). The diagnostics serialization
  `expect` remains; the other non-test-scoped panic finding is a test-only
  notification constructor. No blanket dead-code suppression was present.

## Follow-up: resolved analysis and fixture blockers

- RustQualityLens now has a default-off trusted build-script/proc-macro option,
  correct glob navigation, and local macro-origin selection across all analyzer
  definition locations. Unsupported virtual origins remain unresolved rather than
  falsely external. Cache version 13 invalidates old identities; protocol XML and
  test fixtures are declared fingerprint inputs. Unit and live macro regressions
  cover these paths; no unresolved-reference exclusions were introduced.
- `packaging/hypridle/` is the shared package/patch definition used by Shelllist and
  the new `managedHypridle` flake package. The dev shell and package checks export
  `HYPRIDLE_TEST_BIN`; CI coverage includes the native fixture automatically.
- Sandbox validation exposed two hidden host dependencies: the test now owns its
  D-Bus configuration and home/config directories, and the Hypridle patch honors
  the selected `--config` file instead of looking up an unrelated host default.
  Readiness failures include native child logs; readiness content is asserted.
- After fixture hardening, current cognitive sum/max is **1595/18**, cyclomatic
  **3306/48**, maximum hotspot **161.95**, and Rust physical lines **22196**.
  The original refactor's nine-line reduction is outweighed by 17 additional test
  lines for isolation and diagnostics; this follow-up is a correctness improvement,
  not a claim of net LOC reduction.

RustQualityLens validation: **157 tests** pass across the workspace suite and
explicit live-macro fixture; formatting, strict Clippy, rustdoc, bundled-helper
parity, package smoke, and refreshed evidence-policy checks pass. Its existing
self-metric source budget still fails (**20315 > 17289** nonblank lines; already
20003 before these changes). Maximum hotspot remains **69.61**, below the unchanged
70 limit. This is not a claim that every RustQualityLens self-gate passes.

Logs: `target/review-after/full-fixture-coverage.log`,
`full-fixture-diff-cover.log`, and `nix-package-tests.log`.

Reproduce with the updated local tool, inside the development shell (new source
files must be tracked for the co-development helper's snapshot):

```sh
for tool in hotspots clones escape-hatches reliability leverage locality architecture-rules coverage correctness-run; do
  ../rust-quality-lens/target/debug/rqlens measure "$tool" --config rqlens.toml
done
../rust-quality-lens/target/debug/rqlens review --changed-since HEAD --config rqlens.toml
../rust-quality-lens/target/debug/rqlens check --config rqlens.toml
cargo llvm-cov --workspace --all-targets --locked --cobertura --output-path target/review-after/cobertura.xml -- --include-ignored
diff-cover target/review-after/cobertura.xml --compare-branch HEAD --fail-under 80
```

No fixture skip or manually pinned Nix store path is needed. See
[`packaging/hypridle/README.md`](../packaging/hypridle/README.md).
