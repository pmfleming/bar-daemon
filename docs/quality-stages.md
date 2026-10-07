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
