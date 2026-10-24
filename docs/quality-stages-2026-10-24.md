# Five follow-up quality stages

Baseline: `d8bd09f`, following the initial [RQLens review](quality-refactor-2026-10-24.md).
Each stage was validated and committed independently. The priority here was closing
coverage and lifecycle gaps, not forcing every metric or source-line count downward.

## Stages

1. **Real API dispatch coverage** (`c595970`). Replaced the direct theme-store
   publication test with an isolated `ApiService` scenario. It covers full/subset/
   empty/duplicate snapshot selections, invalid stream requests, pure theme
   resolution versus publication, invalid themes, unknown routes, local mutations,
   and sticky monitor-failure fencing. No monitors or host service effects are started.
2. **Notification route ownership** (`0d3f9d6`). Eviction and confirmed deletion
   previously left entries in the separate internal update-route map. Routes now
   live in `EngineData`; removal retires the active record and its route together.
   Removed the extra mutex and the later opportunistic cleanup pass. A lifecycle
   matrix checks replacement, eviction, deletion, dismissal, clearing, persistence,
   and exactly-once close signals. This fixes stale bookkeeping; it is not a claim
   that the old entries bypassed active-notification validation.
3. **History date parsing** (`a807db6`). Date recognition/validation and date matching
   now belong to `Day`; `Search` assembles terms and avoids constructing search text
   for date-only or rejected matches. Added malformed, Unicode, overflow and calendar
   boundary cases to the existing tests. Bare `yesterday` at the minimum supported
   date now fails explicitly rather than silently becoming a text term.
4. **Validate before catalog I/O** (`fee9532`). A prepared query validates filters,
   limits and date terms before reader admission, preserving one observation instant
   through rendering. Tests assert invalid input never invokes the catalog loader.
   Cursor revision binding still requires the loaded catalog. History queries, log
   lookup and manual log reads share a three-second deadline helper; their individual
   deadlines and error messages are preserved. A timeout test verifies that already
   running blocking work can finish without replay; the existing cancellation test
   verifies it retains its admission permit until completion.
5. **Overview projection ownership** (this commit). Separated system, baseline,
   tool and timer projections, shared bounded field validation, removed temporary
   JSON construction for timer timestamps and an intermediate issue-reference vector.
   Expanded existing tests for report limits, malformed metadata, timestamp endpoints,
   unavailable timer evidence, staging fallback and bounded release metadata.

## Measured results

Same frozen local RQLens executable as the previous review. Final evidence is under
`target/quality-stages/final/`; baseline evidence remains under
`target/quality-refactor/after/`. Stage-specific logs are in `target/quality-stages/`.

| Evidence | Before | After |
| --- | ---: | ---: |
| `src/api.rs` LLVM line coverage | 0% | 93.27% |
| Bar-daemon LLVM line coverage | 71.38% | 72.32% |
| Bar-daemon LLVM region coverage | 69.79% | 70.65% |
| `Search::parse` cognitive / cyclomatic | 15 / 17 | 4 / 4 |
| Search non-test-module function cognitive sum | 24 | 15 |
| Search non-test-module function Halstead effort sum | 67,702 | 55,899 |
| Overview non-test-module function Halstead effort sum | 463,451 | 319,844 |
| `overview::normalize` Halstead effort | 203,343 | 30,512 |
| RQLens duplicated lines | 1,565 | 1,561 |
| Executed tests, including ignored fixtures | 248 | 250 |

Function extraction redistributes work: the smaller `normalize` metric is not the
whole-file reduction. More helper functions increase baseline cyclomatic counts;
for example the overview function sum increases from 37 to 43. Source size also
increases: **+359 Rust lines**, principally explicit boundaries and regression tests.
Two new lifecycle/deadline tests are intentional; this pass does not reduce test count.
Coverage includes test code and excludes the external framework file in the LLVM
export; it is not branch coverage or proof of complete behavior coverage.

RQLens resolved all 2,040 collected final dependency references. Macro expansion
still makes the overall inventory partial; no universal strict quality gate is claimed.

## Validation

Each stage passed its targeted tests and strict all-target Clippy. Final validation:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo llvm-cov --workspace --all-targets --locked --json \
  --output-path target/quality-stages/final/coverage.json \
  -- --include-ignored --test-threads=1
```

**250 passed, zero failed, zero ignored**, including the private PipeWire and native
Hypridle fixtures. The overview's subprocess fixture also still verifies argument
restrictions, malformed/oversized output, nonzero exits and timeout behavior.
