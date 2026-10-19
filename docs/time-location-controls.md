# Manual and automatic location controls (stage 3)

This extends the read-only authority described in `location-timezone.md`.
`GetAccepted` remains compatible. `GetControl` returns authorized mode/revision,
last operation outcome and the effective manual place (if still applicable).
`SetLocationPolicy` is an active-local-session polkit-authorized system-bus method;
it receives `{request_id, revision, place}`. A validated place requests Manual;
explicit null requests Return to Automatic. The real bus caller supplies identity,
never a user-provided UID. The session daemon proxies these through
`location.status`, `location.setManual`, and `location.returnToAutomatic`.

The authority serializes controls with acquisition, fences queued work with the
same control epoch, and checks the native coordinate-derived canonical timezone.
Only it writes timedated. Manual saves intent before effects, verifies readback,
then durably publishes Manual mode and user-selected coordinates. Same-zone choices
still persist without a redundant timezone write. A bounded last-operation record
prevents duplicate effects; stale revisions fail closed. Lost replies require a
status read, never an automatic mutation replay.

Return to Automatic changes durable mode and rearms existing acquisition without
changing the last effective location/timezone. Until fresh automatic acceptance,
manual provenance remains manual. It does not bypass cooldowns, VPN policy, external
manual-change detection or uncertain-write fences. A safety pause requires the
existing explicit administrative reconciliation/reload, not a silent UI reset.

State/coordinates remain private. Public status carries only message and revision.
GetAccepted/GetControl reads are revision-fenced in the session observer. The new
system-bus and polkit policy must be deployed together with the authority; no
service was activated by this implementation. Old authorities remain read-only.

Validation: native authority tests include durable manual/resume, duplicate and
cross-caller requests, same-zone updates, lost write replies, restart, cancellation
of in-flight automatic acquisition, and a real private-D-Bus polkit/reader/control
round trip with denial. Authority suite: 34 passed, one existing packaged-agent
fixture ignored. Library: 154 passed, one existing ignored test. Fixtures use fake
providers/timedated, never the live host. Full package/deployed policy acceptance
is a subsequent gate.
