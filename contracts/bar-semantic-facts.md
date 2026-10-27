# Bar semantic facts — CQ-10 / CQ-13

Bridge remains default; this contract does not approve deployment or rollout.

## Telemetry

`battery.display` is projected by `presentation::battery` at the producer's existing
battery observation boundary. `percentage`, `power`, `health` and `cycles` are
unit-bearing labels. `estimate` is a native duration label or null when unavailable
or plugged without charging. Missing/invalid power is `— W`, including when
`power_available` is false; an available true zero remains `0.0 W`. Zero health
and cycles remain known observations. The raw fields remain available to retained
clients. Rendering must not reconstruct labels from missing numeric fields.

Native `state == "fully-charged"` is the only fully-charged semantic witness.
Plugged 100% may instead be paused/inhibited. QML retains glyphs, paint bounds,
known-only fill animation, warning/critical tone and punctuation. Battery thresholds,
calibration, critical policy and operation admission are unchanged.

Audio/brightness availability and native percentage fields already exist. OSD
consumers require an available finite numeric reading in its native 0–100 range
before labeling it; missing/null/nonfinite/invalid values are unavailable, not 0%.
The geometric clamp remains UI-owned. Keyboard-backlight null is not a reading.

## Manual profile cycle

`power_profile.next_manual_profile` is an optional explicit target (null when no
cycle is possible). The native order is power-saver → balanced → performance →
power-saver, restricted to advertised supported members. At least two distinct
members and a known, advertised current member are required. Extension names remain
observable but cannot become cycle targets or establish a cycle's starting point.

The bar captures this projection once on explicit command and submits the existing
`powerProfile.set`; absent/unknown targets and unavailable/pending state send no
command. New observations, recovery and failures do not cycle or retry. Explicit
retry captures the then-current projection. Existing native publication clocks
fence stale observations independently of effect acknowledgements.

`PowerEnvelope::select_manual` re-reads advertised availability under the envelope
owner before pause persistence, hold release or selection. A removed/unsupported
target fails without changing automation/holds. The power service remains final
authority if availability changes after that read. Existing manual override,
automatic resume, cooperative holds, failure reconciliation and retained external
D-Bus/bridge callers remain supported; cancellation is not rollback.

## Open acquisition-validity row (CQ-10 is partial)

The display contract cannot recover validity already discarded by acquisition.
Source witnesses, not newly executed hardware reproducers:

- `battery/upower.rs::read_state` defaults a missing Percentage to 0.0 and casts
  nonfinite percentage/Capacity values to u8; available/present may remain true.
- `battery/sysfs.rs::read_battery` defaults to 0 when neither capacity nor an
  energy ratio exists. Aggregation can therefore label a missing reading 0%.

These pre-existing paths are unchanged in this checkpoint. A follow-up must carry
native percentage validity (and finite health validity) through acquisition,
aggregation, display and gauge, with matched serializer/Qt fixtures. It must
explicitly preserve conservative warning/critical, calibration and admission
behavior rather than treating invalid telemetry as healthy or casually changing
`available`. Required fake-provider cases: missing/zero/negative/nonfinite
percentage and health, missing sysfs capacity plus energy, and policy/safety
behavior alongside presentation. Do not infer the missing fact in QML. Until
that row passes, this is not complete CQ-10 qualification.

## Acceptance boundaries

Unit/native point-to-point fake-PowerProfiles tests cover eligibility, removed
target and hold preservation alongside the existing automation/failure suite.
Production serializers supply checked frontend protocol/read/control companions.
Actual Qt tests exercise tooltip/gauge/OSD unknown and zero, charge-state marks,
disabled cycling, one captured command, stale publications and explicit retry.
These are private correctness checks, not hardware, installed-session, durable
outcome, performance or complete quality qualification. The separate exact-session
Rust/QML quality review is still required; TypeScript has separate compile/parity
checks and is not certified by either Lens.
