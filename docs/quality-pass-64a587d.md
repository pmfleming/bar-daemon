# Quality pass with committed RQLens

RQLens commit: **64a587d**, generated-symbol resolution with explicit trusted
macro execution. Its worktree is clean. This pass uses one frozen executable
(SHA-256 `ed7e1b4aca1134005540f4d27297de94fa24f053b6c48a1a384f51ff7196b804`)
and identical helper inputs (`b9978256a51995f7`) for both measurements.

The baseline is bar-daemon's working tree **after** the previous refactor and
fixture fixes, not clean HEAD. No previous changes were discarded. Evidence,
original copies of this pass's changed Rust files, and its isolated diff are in
`target/quality-pass/`. Both runs have complete evidence for all 17 producers,
one consistent input fingerprint per run, and the same 81-file source scope.

## Confirmation

- Architecture: **774/774 references resolved locally**, no unresolved/external
  references; all three configured rules pass with zero violations.
- `rqlens verify`: **20 checks passed, 8 optional checks skipped**, no unavailable
  checks or compiler diagnostics. Skips are not counted as passes.
- No configuration, source exclusions, policy thresholds, or exemptions changed
  during this pass. RQLens's separate, pre-existing self LOC budget remains unmet;
  committing its resolver fixes does not make that gate pass.

## Refactors

- **Display layout:** share actual-setting comparison between applying and
  confirming layouts. Compare only required output fields, rather than constructing
  temporary full settings. Separate one-attempt saved-layout recovery from preview
  recovery; failed attempts remain visible until topology/resume changes. Move the
  confirmed proposal into saved state instead of cloning it. Three scalar reads
  use `StateStore::read` instead of copying the entire bar snapshot.
- **Battery history:** move live observations into the current endpoint; clone only
  observations retained as samples. Serialize a borrowed deque using the same file
  DTO/schema instead of copying every retained point into a temporary vector.
- **Wayland:** remove a redundant end-of-loop retry branch while preserving
  readiness handling, I/O error propagation, and the roundtrip timeout.
- **Tests:** extend existing layout tests for every compared field, disabled-output
  confirmation, sticky saved-layout failure, and recovery after another resume.

## Measurements and limits

Numbers include Rust tests unless stated otherwise.

| Metric | Before | After |
| --- | ---: | ---: |
| Layout `tick_at` hotspot / cognitive | 142.07 / 14 | 93.18 / 7 |
| Layout `apply` hotspot / cognitive | 116.20 / 12 | 42.39 / 5 |
| Cognitive sum / maximum | 1595 / 18 | 1591 / 18 |
| Cyclomatic sum / maximum | 3306 / 48 | 3309 / 48 |
| Maximum function hotspot | 161.95 | 161.95 |
| Duplicated lines / clone groups | 1238 / 94 | 1238 / 94 |
| Mean leverage / locality | 66.062 / 97.593 | 66.062 / 97.593 |
| Rust physical lines, `src` + `tests` | 22196 | 22240 |
| RQLens source lines | 20571 | 20606 |

The new saved-layout helper scores 49.95, with cognitive 7/cyclomatic 7. The
largest global hotspot remains the managed lid observer. This is a targeted
simplification and allocation reduction, **not** an aggregate cyclomatic or LOC
reduction: 40 added physical lines are tests and four are production. No measured
throughput, allocation benchmark, or calibrated developer-effort claim is made.
The slightly lower duplication percentage only reflects the larger denominator;
detected duplicate lines/groups did not fall. Production panic findings are
unchanged; the added tests introduce three additional test-only unwrap advisories.

## Validation

- All **121 tests pass**, including the private native-idle and PipeWire fixtures
  with `--include-ignored`, both in dev-shell coverage and sandboxed Nix release
  checks; no tests were deleted or added to the ignored set.
- Formatting, strict all-target Clippy, doctests/rustdoc, configured verification,
  and the architecture policy check pass.
- Full-fixture changed-line coverage: **98% (68/69)** for this pass alone and
  **97% (243/249)** for all uncommitted changes, above the unchanged 80% gate.
  Test files and the C++ fixture are not part of these production-line percentages.
- The two baseline production-scoped reliability warnings remain: diagnostics
  serialization's `expect`, and a test-only notification constructor currently
  classified as production. Broad orchestration still needs stronger integration
  coverage; this pass does not hide those findings or claim a clean reliability gate.

Reproduce in the development shell with the committed local RQLens binary:

```sh
rqlens measure all --config rqlens.toml
rqlens verify --config rqlens.toml
rqlens check --config rqlens.toml
cargo llvm-cov --locked --workspace --all-targets --cobertura --output-path cobertura.xml -- --include-ignored
diff-cover cobertura.xml --compare-branch HEAD --fail-under 80
```

Logs and raw JSON are under `target/quality-pass/{before,after}/`; the narrower
coverage run uses `--diff-file target/quality-pass/current-pass.patch`.
