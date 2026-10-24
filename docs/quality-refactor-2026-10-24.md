# RQLens review and refactoring — 2026-10-24

Baseline: `61e2f1d`. This is a focused refactoring pass, **not a claim that every requested metric improved**.

## Findings addressed

- `activity/presentation.rs`: minute refresh cloned entire forecasts, allocated an intermediate hourly window and a temporary bounds vector, and repeated astronomy projection already done by the service. Forecast projection now mutates in place; bounds stream over finite values; the service copies provider-owned data only at its publication boundary. Removed the redundant `astronomy::weather` wrapper.
- `battery/history_chart.rs`: segment construction relied on several nested `unwrap`s. Point validation and segment insertion now handle missing readings and discontinuities explicitly. Shared `presentation::measurement` owns nonnegative, one-decimal telemetry formatting for battery labels and location accuracy. Seven production `unwrap` calls and four explicit production `.clone()` calls were removed in the changed code.
- `api.rs`: nested theme/update handling obscured dispatch. Activity/location routing now lives in `ActivityApi`; read-only update routing and error classification live in `api/updates.rs`. Theme serialization borrows roles before moving them into state. The existing outer reply fence remains in place.
- Tests: consolidated weather, timezone status, battery-alert and notification assertions into scenario tables; reused existing notification fixtures; replaced the assertion-free ignored history performance probe with assertions over a 1,000-entry catalog in the paging test. Existing oversized-row progress checks remain.
- The initial instrumented baseline exposed a location test clock race: constructing the timezone index between capturing `now` and constructing an IP fix could exceed the five-second future tolerance. The test now supplies one deterministic observation instant to both fixes. Production freshness checks were not relaxed.

## Measurements

Same frozen local RQLens executable for both snapshots; raw function metrics below are syntax measurements, not estimates of developer hours.

| Function | Cognitive before → after | Cyclomatic before → after | Halstead effort before → after |
| --- | ---: | ---: | ---: |
| `ApiService::dispatch` | 24 → 6 | 48 → 27 | 123,619 → 48,259 |
| `activity::presentation::weather` | 8 → 4 | 7 → 4 | 252,606 → 82,051 |
| `activity::presentation::subtitle` | 15 → 8 | 15 → 12 | 38,876 → 25,718 |
| `battery::history_chart::series` | 17 → 6 | 10 → 6 | 98,926 → 53,952 |

Extraction moves some work into smaller helpers; these are not whole-project reduction percentages.

| Repository evidence | Before | After |
| --- | ---: | ---: |
| Sum of function cognitive complexity, including tests | 3,148 | 3,142 |
| Sum of function cyclomatic complexity, including tests | 6,406 | **6,423** |
| Sum of function Halstead effort, including tests | 30,360,534 | 30,176,160 |
| RQLens duplicated lines | 1,628 | 1,565 |
| RQLens duplication percentage | 3.88% | 3.73% |
| Glob imports | 37 | 33 |
| Authored tests inventoried by RQLens | 249 | 247 |
| Assertionless tests inventoried by RQLens | 3 | 2 |
| Executed tests, including ignored fixtures | 250 | 248 |
| Bar-daemon LLVM line coverage | 71.19% | 71.38% |
| Bar-daemon LLVM region coverage | 69.55% | 69.79% |
| Rust physical lines, `src/` + `tests/` | 44,798 | **44,848** |

Coverage includes instrumented test code, excludes the external framework file in the LLVM export, and is not branch coverage. Weather presentation and battery chart files reach 100% line coverage. Executed test counts differ from the pre-expansion RQLens inventory.

## Locality and leverage

The initial baseline semantic run timed out. A second baseline run used a clean, detached worktree of the baseline revision, a 600-second identity budget, and the same executable. It resolved all 2,001 collected references; the final run resolved all 2,026. The temporary worktree was removed afterward.

- API outbound dependencies: **20 → 18**; RQLens locality: **55 → 61**.
- Activity API outbound dependencies: **6 → 7**; locality: **97 → 94**, reflecting ownership of the location authority adapter.
- Astronomy no longer depends on the weather model: outbound dependencies **1 → 0**.
- Shared formatting and test helpers improve reuse within existing dependency edges. Existing touched modules' observed-reuse leverage scores are unchanged; the mean over modules decreases slightly when the new single-consumer update adapter is included. No numerical leverage gain is claimed.

Architecture/type inventory envelopes remain partial for unexpanded macros. Complete resolution of collected references does not make the entire inventory compiler-complete.

## Validation and remaining work

Passed:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --doc --locked
cargo llvm-cov --workspace --all-targets --locked --json \
  --output-path target/quality-refactor/after/coverage.json \
  -- --include-ignored --test-threads=1
```

Final execution: **248 passed, zero failed, zero ignored**, including private PipeWire and native-idle fixtures. Doctest discovery contains zero tests. The initial failed baseline run is retained alongside its successful serialized retry. Full RQLens `verify`/audit/deny and mutation testing were not run.

Unmet goals are explicit: physical Rust LOC increased by 50 (43 production and 7 test lines); aggregate cyclomatic complexity increased; module leverage scores did not improve. Test count decreased, but test source volume did not. Do not describe this pass as a global reduction of all metrics or a completed dead-code audit. The compiler reports no unused-code warnings, but that is not proof that every public/runtime path is useful.

Next priorities are root `ApiService` execution coverage (still absent), location authority orchestration, and notification persistence initialization. Preserve cancellation, authorization and persistence failure tests when consolidating these areas; structural clone matches between unrelated enum mappings are not grounds for sharing their behavior.

## Reproducible evidence

Local artifacts and logs: `target/quality-refactor/` (ignored by Git).

- `before/analysis/`: baseline syntax measurements; `before/coverage.json`: successful baseline coverage.
- `before-semantic/analysis/`: complete baseline reference-resolution measurements.
- `after/analysis/`, `after/coverage.json`: final measurements.
- `before/source/`: archived baseline Rust source; per-phase `rqlens.toml` files preserve measurement settings and output locations.
- `rqlens`: frozen executable built from `../rust-quality-lens`; SHA-256 `b597936551566c0903b33c7e156073de05eaa5ca0fac7f601f345c9a4b309060`.

Producers run: hotspots, clones, escape-hatches, locality, leverage and test-quality on both snapshots; reliability on the final snapshot. The normal project measurement policy was not weakened.
