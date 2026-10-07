# Five-stage quality follow-up

Starting point: `1458892` (the earlier media refactor, checkpointed separately).
The fixture-runner integration in `1debec3` is retained. Baseline full-fixture test
and coverage evidence is saved under `target/quality-stages/baseline-{test,coverage}`.
Every stage is committed separately; generated evidence stays under `target/`.

## 1. Work-area refresh policy

`RefreshPolicy` owns demand/cache/retry decisions; the asynchronous monitor owns
fetch cancellation, notification permits and publication. A read consumes only
pre-existing invalidations, and a cancelled read never marks the cache clean.
Healthy connected idle caches survive until invalidated or observed after expiry.

RQLens `monitor_with`: cognitive **18 → 3**, cyclomatic **12 → 5**, per-function
Halstead effort **69,731 → 38,092**. The extracted pure policy is not free complexity:
`action` is cognitive/cyclomatic 8/6 and `completed` is 3/3.

Validation: eight work-area tests, including exact deadline/recovery cases and
last-reader departure winning over a simultaneously ready result; strict all-target
Clippy and the full-fixture runner. Move the test-only cancellation guard to module
scope so its ownership is explicit to both Rust readers and the analyzer.

## 2. Notification persistence dispatch

Share cancellation-aware response delivery and extract ordered mutation application.
Failed writes still fence every later revisioned catalog query, even after successful
mutations; later accepted writes still run. The legacy list deliberately remains
best-effort (it carries no catalog revision), but now skips cancelled reads too.

RQLens `persistence_worker`: cognitive **17 → 3**, cyclomatic **9 → 5**, effort
**24,446 → 11,906**. Extracted `apply_mutations` is 3/3 and `reply` is 2/4.
Validation: 22 notification tests, including cancelled/fenced reads that must not
execute storage work and a real SQLite trigger failure followed by successful queued
writes; strict all-target Clippy and the full-fixture runner.

## 3. Battery energy integration

`DischargeSegment` validates the two measured endpoints, interpolates power, and
accumulates trapezoids into bounded bins. `energy` retains timeline bounds and result
assembly. Arithmetic order, continuity exclusions, legacy power validity and the
48-bin/15-minute sizing policy are preserved; no new module dependencies are needed.

RQLens `energy`: cognitive **6 → 3**, cyclomatic **11 → 3**, effort
**152,462 → 37,390**. Segment validation/interpolation/accumulation add their own
complexity; combined effort for these four functions is **77,101**.
Validation: four derived-battery tests, including invalid endpoints, no bridging of
unobserved gaps, nonzero origins and `u64::MAX` duration/coordinates; strict all-target
Clippy and the full-fixture runner.

## 4. Weather transport and normalization

HTTP request construction borrows static query fields and the configured timezone.
Response normalization, hourly selection and daily decoding are pure operations with
an explicit clock; region IDs use that same instant. Remove the now-unused implicit-
clock timezone wrapper and borrow today's forecast rather than cloning it.

Keep the 12-hour/7-day limits, inclusive half-hour cutoff, units and short-column/empty-
day defaults. Provider timestamps now use checked seconds-to-milliseconds conversion:
malformed extreme values return an error instead of overflowing in debug/release.

RQLens `fetch` effort **150,549 → 2,820**; normalization **29,584**, hourly selection
**9,811**, hourly conversion **7,426**, daily conversion **11,112**, request construction
**3,014**. This is primarily an effort/ownership improvement, not a reduction in all
branch counts: explicit validation and the bounded hourly loop add decisions.
Validation: five offline tests cover transport parameters/timeout, field mappings,
short columns, empty days, exact cutoffs, horizon limits, labels, malformed JSON and
all timestamp overflow sites; strict all-target Clippy and the full-fixture runner.

## 5. Fresh, full-fixture quality evidence

Add `tools/quality-evidence.sh` and use it in CI. It retains the existing private
fixture runner, then executes verification, every standard measurement and the strict
practice/test/partial/architecture gate. Independent collection continues after errors;
failed runs retain logs and reports but never receive an outer `complete.txt` marker.
Each run uses a copied executable and a fresh output directory. A derived config changes
only root/output paths, verified by parsed equality; policy thresholds, test selection and
architecture rules remain intact. Runner and CI sources participate in fingerprints.
Eleven Python contract tests cover fixture selection and orchestration, failed commands,
partial policy, path quoting, config preservation, binary replacement and output reuse.

RQLens companion commit **`36a2ae1`** adds opt-in `verification.include_ignored` across
verification, correctness, LLVM coverage and repeated tests, including sanitizer/Miri
commands when enabled. Compile-only discovery and doctests retain their own selection.
This project enables it so analyzer evidence includes the same native fixtures as the
explicit runner. **Publish/use this RQLens commit or newer alongside the CI/config change.**
The analyzer was built and tested from an isolated committed worktree to avoid consuming
concurrent compiler-backend edits. Its ordinary test suite and strict all-target Clippy
pass; four optional analyzer integration/live tests remain ignored, not certified.

The upstream lexical-ownership fix in `ab30776` removes the need to move the remaining
block-local DTOs and private fixture guard just to satisfy inventory. Their original
scopes remain. Capture the browser-identity fixture's nested libtest output instead of
inheriting stdout: both child variants still must succeed and failures include their
output, but duplicate child result lines no longer make the parent test status unknown.
The analyzer's conservative duplicate-result handling is not relaxed.

Validation: **202 unit tests + two integration tests**, zero ignored or failed tests;
formatting, strict Clippy, doctests, warning-free rustdoc, cargo-audit, cargo-deny and
cargo-shear pass. Audit still reports a non-failing yanked `chacha20 0.10.1` warning;
no dependency-policy settings or lockfile entries were changed. RQLens reports
**20 passed practice checks, eight explicitly skipped
optional checks, no unavailable checks or failures**. Full-fixture Cobertura line
coverage improves from **68.2% to approximately 69.3%** with identical runner selection.
RQLens's LLVM JSON aggregate uses a different line-count representation; do not compare
its percentage directly with Cobertura. MSRV 1.85, mutation, sanitizers, Miri, fuzzing and
repeated-run flakiness were not certified by this pass.

Evidence: `target/quality-stages/stage5-validated` records pre-commit validation;
`stage5-committed` is the post-commit refresh. Each contains its exact config, binary hash,
revision/status, phase logs, full-fixture coverage and `analysis/` artifacts. Earlier
`stage5-evidence` and `stage5-final-evidence` directories are investigation snapshots,
not the final certification result. Stages 1–4 retain their original analyzer snapshots;
do not count changed analyzer inventory as an application-code metric improvement.

**The strict gate remains blocked, not waived.** Observed architecture references all
resolve (1,120), with zero observed rule violations. Nevertheless, state-update/Wayland
macro-generated bodies and imported implementation owners remain incompletely inventoried;
type-health also lacks complete cfg/derive coverage. An exploratory expansion probe received
all eight enumerated replies but explicitly did not establish semantic inventory completeness.
Fresh standard artifacts retain these limitations. Incomplete architecture/correctness inputs
block the architecture, partial and test-failure policies even when all executed tests pass.
No generated bodies are suppressed, no tests removed, and no thresholds raised to obtain green.
