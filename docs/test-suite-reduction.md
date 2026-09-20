# Test-suite reduction

## Baseline and result

This is a one-time reduction of **bar-daemon's Cargo-registered Rust tests**,
not a permanent limit on future tests. Shelllist/QML tests are outside this
repository's baseline and were not changed.

Baseline: `5f15af0`, measured with `cargo test --locked --offline --all-targets -- --list`.
Counts include ignored tests, so ignoring tests cannot satisfy the target.

| Measure | Before | After |
| --- | ---: | ---: |
| Registered tests | 181 | **121** |
| Library tests | 178 | 119 |
| Startup integration tests | 2 | 1 |
| Native Hypridle integration tests | 1 | 1 |
| Opt-in/ignored declarations | 2 | 2 |
| Physical Rust lines (`src` + `tests`) | 22812 | 22188 |

`round(181 × 0.67) = 121`: **60 fewer tests**, a **33.15% reduction**, retaining
66.85% of the starting count. No tests were newly ignored or excluded. Removed
test functions were deleted, not renamed into helpers invoked by a giant test.
Production code is unchanged; test/support code is 624 Rust lines smaller overall.
This is not a claim of 33% fewer exercised cases or faster execution. Consolidated
scenarios lose individual case selection and stop at the first failed assertion.

## Selection and retained coverage

Compatibility-only serialization/default migration checks, private-lock probes,
an arbitrary LED polling-interval assertion, and source-vector ordering constraints
were removed. Repetitive valid/invalid cases now share their domain's existing
fixture or lifecycle scenario. Concurrency, persistence failure, restart recovery,
real wire contracts and safety-critical regressions remain independently visible.

The following table accounts for all 60 fewer registrations. Counts for all
other files are unchanged.

| Test module/file | Before → after | Retained behavioral witness / reason |
| --- | ---: | --- |
| `brightness/tests.rs` | 12 → 6 | Requested-value stepping, coarse/large ranges through the service, invalid hardware, isolated root/device selection, helper success/failure/timeout, concurrent publication. Removed direct gate probing and permission tests that could silently skip as root. |
| `sleep_tests.rs` | 16 → 10 | Parameterized final cancellation/unlock/session-switch races, missing/failed locks that must not invoke setup, unreadable/already-preparing states, healthy/broken post-dispatch telemetry. Invalid actions remain checked before valid manual actions. The generic deadline/drop assertion now shares the monitor cancellation fixture. |
| `state.rs` | 7 → 4 | Subscription ordering and duplicate suppression share the concurrent publisher test. Resume generation survives both conditional and unconditional stale telemetry. Independent lid state and late job failures retain dedicated tests. |
| `battery/history.rs` | 6 → 3 | Observation-gap/clock-reversal matrix, live endpoint plus bucket accumulation, persistent restart plus charging transitions and retention pruning. |
| `battery/config.rs` | 3 → 2 | Dropped legacy migration-only checks. Exact calibration restoration and configuration range validation remain. |
| `battery/derived.rs` | 4 → 2 | One energy integration/invalid-input scenario and one charge/discharge forecast lifecycle; zero, NaN, infinity, gaps, bounded estimates, limits and AC changes remain covered. |
| `battery/helper.rs` | 2 → 1 | The same fake battery checks target traversal, thresholds, readback and rejected charge modes. No dependence on the exact ordering of advertised modes. |
| `battery/levels.rs` | 2 → 1 | Critical KeepCurrent independence is checked at the critical transition in the hysteresis test. |
| `battery/policy.rs` | 4 → 2 | Startup suppression, disabled-alert consumption, AC and hysteresis rearming share a discharge lifecycle; protected charge completion remains separate. |
| `battery/mod.rs` | 3 → 2 | Managed baseline thresholds are checked before calibration takes ownership; restoration cannot reopen thresholds. Charge-once restoration remains separate. |
| `api/battery.rs` | 3 → 2 | Partial updates, independent modern profile actions, invalid enum/range/empty requests share one contract test. Atomic optional protection ranges remain separate. Removed the legacy alert-policy API update. |
| `activity/ics.rs` | 3 → 2 | Folded text is parsed through a complete event with a nested alarm, rather than asserting the line-unfolder implementation. Malformed/truncated calendars remain separate. |
| `activity/service.rs` | 6 → 3 | Missing-parent creation, restart, CRUD and range validation join timezone/DST queries. Expired-event exclusion and ongoing-before-future selection are checked through multiple actual ICS providers using dates relative to now. Removed the requirement that an internal source vector never be reordered. Load failure/data preservation and source recovery remain covered. |
| `activity/notifications/engine.rs` | 6 → 4 | Replacement/summary checks join snooze/group lifecycle; timed DND and close-reason expiry share the expiry worker. Concurrent durable mutations and stopped persistence remain separate. |
| `activity/notifications/persistence.rs` | 4 → 2 | Transient exclusion joins ordered persistence/restart. Stopped-worker queries are asserted through the engine alongside mutation rejection. Queue backpressure and cancelled accepted writes remain separate. |
| `media.rs` | 4 → 2 | Seek bounds join the isolated MPRIS wire test. Automatic ranking is exercised through publication before manual selection is checked for persistence. |
| `osd_hardware.rs` | 3 → 1 | One fake LED lifecycle checks values, rereads, missing paths, zero maxima and removal. Dropped private cache/watch counts and a fixed polling-interval ceiling. |
| `paths.rs` | 2 → 0 | Thin atomic-write adapters duplicate the framework's primitive tests. Domain tests still exercise durable policy, history, calibration and layout recovery. The owning framework's atomic abort/commit/failed-replacement test was explicitly rerun. |
| `power/automation.rs` | 3 → 1 | Removed plain Runtime serde roundtrip and direct internal-observer mutation. Real fake-D-Bus restart/manual-pause tests now include hysteresis recovery. Notification/profile independence remains separate. |
| `sleep/outcome.rs` | 5 → 3 | Cancellation phase and stale guard checks join FD lifetime cases. Tracked/untracked late jobs share the job-correlation scenario. Acceptance versus completion and late failure remain separate. |
| `sleep/wayland_lock_tests.rs` | 3 → 2 | Initial locked/unlocked states, live transitions and disconnect share a compositor lifecycle. Missing protocol remains separate and fails closed. |
| `sleep_policy/critical.rs` | 5 → 3 | Disabled default, complete warning interval, delivery failure, no retry and persisted latch restart share a warning lifecycle. Cancellation/AC/unknown evidence and boundary/hysteresis tests remain separate. |
| `sleep_policy/hypridle.rs` | 4 → 2 | Episode validation joins the live-control wire test; invalid/ambiguous configs join the render contract. The native process/inhibitor/base-listener integration test is retained. |
| `sleep_policy/lid.rs` | 5 → 2 | Lid edge safety and profile revalidation remain separate. Never-mode and rejected actions join profile validation. Removed old-policy serde roundtrips and a private policy-mutex test that only called a StateStore setter, not the lid observer. |
| `sleep_policy.rs` | 6 → 3 | Changed, unchanged, shared and equivalent profiles share the setup-race matrix. Hidden-profile bounds join durable policy rollback. Native readiness remains separate. Removed getter-only and primitive unsigned-serde checks. |
| `sleep/keep_awake.rs` | 4 → 3 | Idempotency, reacquisition and EOF release share a healthy/broken-telemetry matrix. Ownership discrimination and denied/preparing inhibition remain separate. |
| `display_policy/layout/tests.rs` | 9 → 7 | Expiry/monotonic/restart/resume recovery includes durable policy blocking and failed rollback. Explicit user cancellation joins token-bound confirmation. Hotplug identity, fallback ordering, invalid layouts, sleep deferral and stable topology remain covered. |
| `tests/idle_startup.rs` | 2 → 1 | Missing executable and immediate process exit use the same readiness failure matrix. |

## Coverage review

Both baseline and final coverage runs used the same command/options:

```sh
nix develop . --offline --no-write-lock-file -c \
  cargo llvm-cov --locked --offline --all-targets --json \
  --output-path target/test-reduction-after/coverage.json
```

Those coverage runs use the normal suite (two opt-in tests ignored in **both**
runs). The separate include-ignored execution validates all 121 tests.

- Whole instrumented line coverage: **8969/15183 (59.07%) → 8629/14840 (58.15%)**.
  Removing covered test code changes this denominator; this is not advertised
  as an overall percentage improvement. Uncovered instrumented lines: 6214 → 6211.
- A separate fixed-location comparison used baseline RQLens function spans to
  exclude test modules/files and cfg(test) helpers. All **752 selected non-test
  function bodies** are unchanged. Of the same **18,218 source-region locations**,
  **8199 → 8216** are covered.
- Reviewed all 12 formerly covered regions that lost coverage: seven belong to
  legacy battery migration/API behavior, two to private brightness zero-maximum
  arithmetic guards (the public service rejects that hardware), and three to the
  thin atomic-write adapter's error-context formatting. The framework primitive's
  failure-preservation test passes. Modern independent profile updates remain
  tested; no production compatibility behavior was removed.
- There are 29 newly covered regions. Aggregate gains alone were not accepted as
  proof: review specifically restored manual preview cancellation, bounded
  preflight resource cancellation, and healthy-telemetry inhibitor release cases.
- Region/line execution is not branch or mutation coverage and cannot prove all
  hardware behavior. No coverage exclusions, gates or production code were changed.

## Verification and artifacts

- Strict all-target Clippy and formatting pass.
- **121/121 tests pass** with `--include-ignored`, including native Hypridle and
  PipeWire tests against private mock buses/sockets and virtual devices.
- The normal library suite also passed single-threaded (118 passed, one opt-in
  test ignored), in addition to default concurrent execution.
- The shared framework's
  `file::tests::staged_abort_commit_and_failed_replacement_clean_up` passes.
- Local before/after test lists, coverage exports, retirement names, logs and the
  fixed-location comparison are under `target/test-reduction-{before,after}/`.
  These are ignored evidence artifacts; `git diff 5f15af0` identifies every change.

No real sleep/hibernate, live power-policy or service activation was performed.
Unrelated work in sibling repositories was left untouched.
