# RQLens review — 2026-10-13

Baseline: `6a947fc8d29a8e47ef48fc0de3e2083fa409efed`.
Tool: pinned copy of `../rust-quality-lens/target/debug/rqlens`.
Local evidence: `target/quality-refactor-2026-10-13/{before,after}`.

## Findings and changes

- `src/api.rs`: routing mixed snapshot projection with domain actions. Extracted
  snapshot handling and delegated battery/sleep routing to their owning modules.
  Snapshot serialization borrows state; stream decoding borrows names from JSON.
- `src/api/battery.rs`: repeated early-return matches obscured persistence and
  rollback order. Use `Result` propagation and shared persistence/error helpers;
  retain the effects lock, rollback behavior and outcome-unknown fences. Validate
  borrowed state rather than cloning whole battery/profile snapshots.
- `src/api/effects.rs`, `src/api/sleep.rs`: sleep routing/publication no longer
  depends on the desktop control bundle. The sleep module borrows the existing
  store instead of introducing another state-owning service.
- `src/daemon/tasks.rs`: declare failure domains beside task registration, removing
  the parallel name/domain match and redundant `Monitors` wrapper.
- `src/state/events.rs`: pass the matched watch receiver directly to the shared
  coalescing calculation; remove an unreachable variant check.
- `src/protocol.rs`: derive stream metadata from `STREAMS`, removing a second
  registry, temporary name copies and two fallible JSON shape assumptions.
- Replace six glob imports with explicit imports. No new suppressions or macros.

## Measurements

Function totals below cover the eight changed/new Rust files, including tests
and extracted helpers, not just functions that became smaller.

| Signal | Before | After |
| --- | ---: | ---: |
| Cognitive complexity sum | 159 | 111 |
| Cyclomatic complexity sum | 339 | 310 |
| Halstead effort sum (rounded) | 2,101,988 | 1,778,878 |
| API dispatch cyclomatic complexity | 46 | 29 |
| Monitor supervision cyclomatic complexity | 20 | 3 |
| RQLens duplicated lines, whole project | 1,934 | 1,896 |
| RQLens escape occurrences, whole project | 61 | 55 |
| Physical Rust lines, `src/` + `tests/` | 39,324 | 39,307 |
| Literal `.clone()` calls, `src/` + `tests/` | 649 | 646 |

Desktop effects' outbound dependencies fell from 18 to 13 and locality risk from
39 to 24. The new sleep module has risk 9; API-root risk rose from 35.25 to 36.
Observed module-level leverage scores did **not** improve. Shared helpers improve
within-module reuse, but adding artificial consumers to raise scores is unwarranted.
No compiler-reported dead code or unused dependencies were found; do not mistake
unreferenced public contracts or generated bindings for removable code.

## Validation and limits

- `tools/quality-fixtures.sh test`: 308 unit tests and two integration tests pass,
  including private PipeWire and native-idle fixtures; zero ignored tests.
- Formatting, strict all-target Clippy, doctests and `cargo shear` pass.
- Extended existing tests cover empty/duplicate/all-stream projections, unknown
  delegated routes, and watch coalescing across async and nonblocking reads.
- Lens commands: `measure hotspots`, `clones`, `escape-hatches`, `locality`,
  `leverage`, using the before/after configurations in the evidence directory.
  Evidence remains partial for unexpanded macros/generated bindings; scores are
  advisory. No coverage, mutation, audit or full strict-policy claim is made.
- Concurrent edits in the local `../daemon-framework` dependency caused some
  measurement attempts (including the extra architecture-rules run) to abort
  their freshness checks. The five reported producers published successfully;
  their evidence describes those captured inputs, not subsequent dependency edits.
  Dependencies were neither modified nor reverted by this refactor.
- Remaining hotspots include lid connection orchestration and Wayland roundtrips.
  They were left alone rather than splitting safety-sensitive state machines
  merely to lower per-function scores.
