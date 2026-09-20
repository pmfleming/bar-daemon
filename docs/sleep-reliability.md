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
