# Test pruning — 2026-10-13

## Result

| Suite | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| Rust (all targets, including isolated fixtures) | 310 | 208 | 102 |
| Python quality-tooling tests | 11 | 7 | 4 |
| **Total** | **321** | **215** | **106 (33.0%)** |

Counts are registered test functions, not assertions or table-driven scenarios.
Rust source under `src/` and `tests/` decreased from 39,307 to 36,319 lines
(2,988 fewer). This pass changes tests only; the pre-existing quality refactor
remains intact and uncommitted.

## Selection

- Removed overlapping DTO/serialization and fixture self-checks; retained
  `protocol::tests::checked_contract_fixture_is_current`, which checks the
  complete published contract artifact.
- Pruned legacy compatibility examples, repetitive projection/normalization
  checks, and implementation-specific timing, caching, and coalescing checks
  where broader retained tests exercise the behavior.
- Consolidated related sysfs, helper, calendar, location, diagnostic, Wayland,
  and tooling scenarios to share setup. Python subtests still exercise missing
  prerequisites/fixtures, ignored tests, failed commands, evidence preservation,
  and refusal to reuse output directories.
- After coverage review, incorporated snapshot parsing into the Hyprland monitor
  test and kept sleep-route rejection, idle-readiness, and lid-policy
  cancellation cases within related retained tests.

Retained safety-focused suites include authorization/owner-loss fences,
threshold rollback, notification persistence and cursor validation, lock
confirmation and concurrent unlocks, display recovery, update crash/PID-reuse
handling, and state publication races. Both integration tests and the isolated
PipeWire fixture remain; no new ignored tests were introduced.

## Validation

All passed:

```sh
QUALITY_OUTPUT_DIR=target/test-pruning-2026-10-13/final \
  bash tools/quality-fixtures.sh coverage
cargo test --workspace --all-targets --locked -- --list
cargo test --workspace --doc --locked
python3 -B -m unittest discover -s tools -p 'test_quality_*.py'
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The isolated run executed 206 Rust unit tests and two integration tests, with
zero failures or ignored tests. Seven Python tests passed; there are no doctests.

## Coverage and limitations

Reported Cobertura line coverage changed from **79.19%** (18,044/22,785) to
**77.04%** (16,499/21,417), a **2.16 percentage-point decrease**. These reports
include test code and have different denominators; they are not a like-for-like
production-only comparison. The smaller suite deliberately has fewer checks;
passing tests and line coverage do not establish equivalent defect detection.
No branch-coverage or mutation-testing claim is made.

Local evidence is in `target/test-pruning-2026-10-13/`: the starting source
archive and test inventory, baseline tooling copies/logs, `before/` and
`final/` coverage reports, final validation logs, `coverage-summary.json`, and
`pruning.diff` (relative to this task's starting tree, not the earlier commit).
The intermediate `after/` report predates the final coverage-driven adjustments.
