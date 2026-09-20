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
