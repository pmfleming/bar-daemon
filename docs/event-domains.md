# Bar event domains (architecture P5)

## Delivery and compatibility

There is no shared all-domain event ring. `StateStore` keeps a source for each
registered stream. Subscribers register only their requested sources, under the
same snapshot lock used by commits, before receiving initial state. Duplicate
stream names are deduplicated. Owner cancellation drops the forwarding JoinSet
and all its domain receivers (including the work-area demand token).

* Battery, power-sleep, active notifications and hardware OSD retain bounded
  128-entry broadcasts: operation phases, notification lifecycle and hardware
  edges can matter. A lag signal names only that receiver's stream.
* The other streams are complete, replaceable state on watch channels. Slow
  readers receive the latest state; skipped intermediate snapshots are measured
  as **coalescing**, not an operation-history gap.
* Watch cursors are internal `u64` revisions, not timestamps or JSON numbers.
  Cross-domain ordering is intentionally unspecified. Each domain's initial
  state precedes its changes.

The v1 event envelope and full `bar.snapshot {}` reply are preserved. The
additive `bar.snapshot {"streams":["battery.changed"]}` option returns the
usual `data.snapshot`, containing only the requested fields. Invalid stream
lists fail validation. The registry documents delivery and recovery semantics.
Old clients can still request full snapshots; old daemons ignore the new option,
so new clients also project replies locally.

`BarDaemonBackend` is shared by bar, battery, activity, notifications, display and
work-area/compositor consumers. It admits at most one snapshot read per domain
per consumer. Repeated gaps during a read invalidate that read and queue at most
one subsequent read, never a parallel full-snapshot storm. A newer full-state
push satisfies recovery. Local per-domain event epochs fence startup and resync
replies; the existing session transport fences generations. This is conservative
v1 fencing, not a claim of a new durable server cursor or cross-restart history.
Recovery emits a synthetic subscribed projection, never a user-change OSD or a
replayed effect. Read failures release the flight and log failure without a tight
retry loop; a later gap, explicit refresh or reconnect retries the read.

## Health and failure policy

The 18 top-level monitor futures are supervised for panic and early return.
Failure logs the monitor name and marks its owned domains unavailable, preserving
last observations and adding an error. This overlays events, initial state and
snapshot/projection reads, including late action completions; a dead monitor
cannot silently become healthy again. Other domains remain alive.

Failure is **sticky until daemon restart**. Do not blindly restart a policy
engine after panic: external effects, notification ownership or inhibitors may
have unknown outcomes. Normal shutdown cancels and joins tasks without falsely
reporting failure. Monitor-running telemetry is not proof of fresh hardware data,
and this is not a heartbeat detector for hung futures or nested/native workers.
Their existing source-specific availability/reconnect logic remains authoritative.
Process/bridge loss is separate: QML marks subscribed domains unavailable and
reconciles in a new transport generation without replaying effects.

## Measurements and checks

Structured tracing records domain publication count and payload bytes (including
cumulative totals), skipped/coalesced snapshots, subscription lag, snapshot request
count, request domains, reply bytes, latency, and monitor availability. Enable
`bar_daemon=trace` for publication counters or `bar_daemon=debug` for recovery and
coalescing measurements. QML exposes per-consumer snapshot request/failure, gap
and discarded-reply counts on `requestPolicy` and logs read latency/errors.
Counters describe in-process publication/delivery, not total D-Bus wire overhead.

The deterministic paused-receiver baseline publishes 1,024 audio values: the old
capacity-128 shared ring reports 896 lost events; the watch reader coalesces 1,023
values to the latest, while battery/notification receivers receive neither audio
nor gaps. A separate 256-event battery burst reports 128 lost transitions only
on battery. This is a saturation experiment, **not** a claim that ordinary audio
or workspace use overflows a channel. Real peer-D-Bus signal tests exercise the
production domain forwarder; native QML tests exercise generation loss, stale
reads and 100 repeated gaps. The monitor-failure QML fixture comes from the actual
StateStore failure publication path.

Shared D-Bus/JSONL/bridge capacity and the snapshot commit lock remain shared
resources. Global congestion is still possible and must retain the bridge's
existing observable failure/reconnect behavior. Domain isolation does not promise
zero shared backpressure or bounded notification payload bytes.

## Process extraction decision

No service extraction is justified by this deterministic experiment alone. Keep
one notification D-Bus server/database owner, battery alert sinks connected to
that owner, one sleep-policy/inhibitor engine, coordinated display/resume policy,
and the existing update/timezone ownership. Extract only after deployment
measurements establish a reliability, lifetime or privilege benefit and a
separate design specifies bus names, dependencies, state migration and rollback.
No service packaging, live policy or service restart is changed by P5.
