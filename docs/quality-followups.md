# Committed quality follow-ups

The four follow-ups were implemented as separate steps after checkpointing the
previous refactor, giving five commits. Existing changes to `flake.nix` and
`docs/location-timezone.md` were deliberately excluded.

## Steps

1. `fc69be7`: checkpoint the location/timezone refactor documented in
   [quality-refactor-review.md](quality-refactor-review.md).
2. `41fe50a`: separate GeoClue setup, waiting, and fix validation. Retain a
   unique-owner manager proxy for bounded cleanup; move timestamp values instead
   of cloning them. The new private-bus regression replaces the service owner
   during acquisition and verifies rejection and cleanup at the original owner.
3. `0be24c1`: group the lid inhibitor, workers, edge detector, retry chain, and
   published status under one controller. Separate completion handling from
   observation; centralize retry eligibility/policy invalidation. Tests cover
   retry invalidation, error clearing, worker cancellation, and FD release.
4. `a1e63f6`: put notification deadline advancement and wakeup selection on the
   notification model. Share quiet-mode expiry across startup and runtime.
   Tests distinguish future snoozes, simultaneous wake/close, popup-only changes,
   disabled-policy cleanup, idempotence, and revision wrapping.
5. This report's commit: fix the three pre-existing formatting failures and
   complete full validation. Final review also orders lid controller fields so
   worker cancellation begins before the native inhibitor FD is released.

## Measurements

RQLens 0.1.0, Rust 1.95, identical configuration and scope. Baseline is the
checkpointed first pass; intermediate hotspot snapshots are retained per step.

| Original hotspot | Cognitive | Cyclomatic | Function Halstead effort |
| --- | ---: | ---: | ---: |
| GeoClue `locate_on` | 17 → 1 | 38 → 16 | 137,907 → 31,413 |
| Lid `connected` | 16 → 2 | 28 → 12 | 205,392 → 78,279 |
| Notification `EngineData::expire` | 16 → 11 | 12 → 9 | 31,230 → 16,549 |
| Notification `build` | 8 → 5 | 11 → 9 | 46,642 → 33,379 |

The extracted GeoClue wait loop has cognitive 15 / cyclomatic 13; the lid
observation method has 12 / 15. Complexity was not eliminated merely by moving
it out of the original functions.

| Whole-project measurement, including tests | Baseline | Final |
| --- | ---: | ---: |
| Function count | 1,613 | 1,625 |
| Sum of function cognitive complexity | 2,711 | 2,708 |
| Sum of function cyclomatic complexity | 5,628 | 5,640 |
| Sum of function Halstead effort | 25,933,816 | 25,952,672 |
| Sum of file Halstead effort | 153,062,965 | 154,716,182 |
| RQLens source lines | 36,798 | 36,999 |
| Duplicated lines | 1,497 | 1,508 |
| Mean locality / leverage | 96.54 / 27.86 | 96.54 / 27.86 |
| Production reliability findings | 96 | 96 |
| Test reliability findings | 897 | 911 |
| Escape-hatch sites | 39 | 39 |

**Trade-off:** this follow-up improves the main functions and removes three
production `.clone()` calls plus the timestamp `try_clone()` in GeoClue, but it
does **not** meet the proposed net-LOC reduction goal. Explicit lifecycle
structure and three new regression tests increase size and some aggregate
metrics. No individual module's locality score regressed. The remaining static
production findings were not suppressed or treated as verified failures.
RQLens confidence remains partial for macro/ownership inventory limitations;
these metrics are advisory, not compiler-complete correctness evidence.

## Validation and evidence

- GeoClue step: 35 tests passed, one existing environment-dependent test ignored;
  strict binary Clippy passed.
- Lid step: 16 sleep-policy tests passed; strict library Clippy passed.
- Notification step: 22 notification tests passed; strict library Clippy passed.
- Final private-fixture suite: **199 passed, zero failed or ignored**.
- Final `cargo fmt --all -- --check`, strict all-target Clippy, and
  `git diff --check`: passed. Doctest command passed with zero doctests.

Evidence is in `target/quality-followups/`: `baseline/`, `geoclue/`, `lid/`,
`expiry/`, `final/`, matching TOML configuration snapshots, per-step test/Clippy
logs, and `fixtures/complete.txt`. Reproduce the full suite with
`bash tools/quality-fixtures.sh` in the development shell. Rerun each RQLens
producer (`hotspots`, `clones`, `escape-hatches`, `reliability`, `locality`,
`leverage`) with `--config target/quality-followups/final.toml`.
