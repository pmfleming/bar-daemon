# Sleep reliability follow-up

This work uses systemd/logind for sleep and never retries a possibly dispatched
sleep request. Development tests use isolated services, not live power actions.

## 1. Bounded dependencies and independent lid observation

Action/policy system-bus connections have a three-second connection/method
budget. Pre-dispatch sleep preparation has a twenty-second total budget;
post-action telemetry has three seconds and cannot turn acceptance into failure.
User-systemd connections/methods are bounded too. A dispatch timeout remains an
ambiguous outcome, not permission to replay the request.

Lid observation no longer acquires the policy transaction mutex or waits for a
lock/helper operation. One owned action worker runs alongside observation;
connection loss cancels it. Session changes can release the lid inhibitor while
a policy save is busy. The state store publishes lid ownership independently so
stale policy refreshes cannot overwrite it. Dependency failures release ownership
and restore logind policy. A two-second fallback supplements property signals.

Validation: 46 sleep-filtered Rust tests passed, including dependency cancellation
and independent lid-state publication under a held policy mutex.

## 2. Live Hypridle listener updates

The paired Shelllist native patch exports a bounded SetTimeout/GetState interface
and owns one separate sleep listener. Changes never restart Hypridle, its
lock/DPMS listeners, or its screensaver inhibitor-cookie owner. Equal timeouts
are no-ops. Each changed timeout invalidates the old countdown generation.
The daemon verifies live process/generation/timeout state instead of treating a
stale generated file as evidence of a running timer. Failed updates roll back
policy and attempt to restore the previous live timeout. Activate both updated
packages together; old native integrations are reported unavailable.

Validation: patched Hypridle 0.1.7 built successfully; isolated D-Bus tests cover
unchanged, changed and Never timeouts with the same PID. Configuration rendering
retains lock/DPMS listeners without adding a competing sleep timer.

## 3. Activity-cancellable idle episodes

The native listener now supplies an episode token as well as a process/countdown
generation. Input reported by any listener, new inhibition, or timeout changes
invalidate that token synchronously in Hypridle, independently of daemon policy
transactions. GetState synchronizes with the compositor before reporting a live
idle episode. Missing/dead processes fail closed. The daemon checks the episode
at entry, around privileged setup, and once more after its final logind queries.
Callbacks waiting more than three seconds for the policy mutex are discarded.

Validation: 49 sleep-filtered Rust tests pass, including activity/inhibition,
replacement-episode and final-preflight cancellation; patched Hypridle builds.

## 4. Support, authorization and inhibition

All seven logind capability values are classified explicitly. Only `yes` may
execute; authorization-required actions are disabled/explained rather than
locking and failing. Unknown values fail closed. Configuration support includes
transiently inhibited and authorization-required states. `hibernate_ready`
separately reports current action readiness and a non-interactive helper polkit
probe; restrictions do not prevent saving otherwise supported profiles.

Validation: capability matrix and isolated logind tests prove denial occurs
before Lock, plus frontend action/description checks and updated API fixtures.

## 5. Lid profile revalidation

Lid profile actions re-read power source and effective hibernate delay after
locking, after privileged setup, and after final logind preflight. A changed
selection cancels that close, without automatic retries. Shared/equivalent
hibernate profiles remain valid (idle timeout is irrelevant to lid close).
Direct lid suspend/hibernate also rechecks the trigger at the final barrier.

Validation: AC removal/insertion, policy changes and equivalent/shared profiles
are covered by unit tests, alongside the existing final-trigger failure test.

## 6. Accepted versus completed sleep operations

`power_sleep.operation` retains an ID, action, phase, correlated systemd job and
error. Phases distinguish requested, dispatching, accepted, preparing, returned,
completed, failed and unknown. PrepareForSleep(false) alone is not proof of a
successful sleep job. An independently polled monitor subscribes to ordered
systemd JobNew/JobRemoved signals and matches the expected sleep service/job.
Later job failures survive telemetry refreshes and appear in the UI with journal
guidance. Missed/unmatched results become unknown after 120 awake seconds; a
monitor disconnect or uncertain method reply also exposes uncertainty. No jobs
are replayed. Cancellation before dispatch is failed; interrupted dispatch is
unknown. The frontend hides Retry for unknown outcomes. Completion means the
systemd service completed, not proof of firmware residency or durable resume.

Validation: transition/correlation/cancellation tests, stale-result preservation,
frontend uncertainty/late-failure presentation checks, and updated API fixtures.

## 7. Runtime override ownership, readback and cleanup

The privileged helper replaces sticky writes with an exclusive FD lease. It
refuses non-regular/admin-owned files, installs only bounded generated contents,
and verifies effective drop-in precedence using a fixed, bounded systemd-analyze
command (packaged in its PATH). Conflicting overrides abort setup. Equivalently
spelled administrator durations are conservatively reported as overrides rather
than interpreted by a second time-expression parser.

Operation completion/failure/uncertainty releases the daemon's FD; daemon death
also closes it. The helper waits at least 30 awake seconds from acquisition and
confirms logind is not preparing and no relevant systemd service/job is active
before removing its unchanged file. It checks abandoned leases after 120 awake
seconds and defers on unreadable state. Startup/periodic maintenance reclaims
orphaned and legacy generated files; disabling hibernation requests cleanup.
No administrator-owned or subsequently edited contents are removed. The global
nature of systemd sleep.conf remains: other callers can share the effective
delay during a lease, and an administrator can still change settings after the
readback barrier. There is no claim of a per-request systemd configuration API.

Validation: temporary-directory ownership/compare-before-delete tests and
readback precedence/reset tests pass. No live /run settings were written.

## 8. Opt-in awake critical-battery protection

`policy.critical_battery` defaults to `{enabled:false, percent:5,
grace_seconds:60}`. Thresholds are 1–20% and warning periods 30–300 seconds.
The dedicated `powerSleep.setCriticalPolicy` API and UI controls work even when
managed idle is unavailable; `powerSleep.cancelCritical` cancels the pending
battery episode without taking the policy mutex. Cancellation cannot undo an
already dispatched logind request.

Fresh aggregate UPower battery/AC evidence drives a monotonic awake countdown.
A critical notification must be delivered before the full grace period starts.
After that there is at most one Hibernate attempt, through normal authorization,
confirmed locking, active-local-session and inhibitor preflight. Policy, AC,
percentage and cancellation are rechecked after locking and just before dispatch.
Known competing desktop power managers and UPower's action-level emergency state
prevent takeover. There is no shutdown fallback, inhibitor bypass or automatic
retry. Hardware that cannot hibernate remains visibly blocked.

Attempts/cancellations are durably recorded in the XDG state directory before
dispatch and survive daemon restart. AC or recovery at least three percentage
points above the threshold rearms an episode; explicit policy changes start a
new policy episode. Unreadable battery evidence cancels pending deadlines and
requires a fresh full warning. Unreadable recovery state fails closed. Do not
enable another desktop's critical-battery policy alongside this one; detection
of known bus names is a safeguard, not a guarantee against arbitrary scripts.
This protects while awake, not ordinary suspended RAM; use systemd's combined
sleep policy for asleep battery protection.

Validation: pure countdown/cancellation/AC/unknown-data/hysteresis/restart tests,
frontend bounds/dispatch checks, API fixture checks and the offscreen BatterySleep
QML suite (10 passed). No live battery policy or power action was exercised.
