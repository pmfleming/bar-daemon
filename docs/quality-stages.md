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
