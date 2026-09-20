# RQLens-guided refactoring review

## Baseline and method

This pass starts **after the preceding audit fixes**, including their uncommitted
regression tests. Comparisons are against that working tree, not `HEAD`.
The local tool was found at `../rust-quality-lens/target/debug/rqlens` (0.1.0),
using complexity model v2 and risk model v4. No scoring rules or exclusions were
changed. Raw evidence and the initial source copy are in `target/refactor-before/`;
updated evidence is in `target/refactor-after/` (both ignored build artifacts).

Producers run: `hotspots`, `clones`, `leverage`, `locality`, `escape-hatches`,
`reliability`, `test-quality`, `architecture-rules`, and `correctness-run`.
Reproduce a producer with:

```sh
../rust-quality-lens/target/debug/rqlens measure hotspots --config rqlens.toml
```

## Results

Totals include tests; source files were not moved or excluded to improve scores.

| Metric | Before | After |
| --- | ---: | ---: |
| Cognitive complexity: sum / maximum | 1505 / 31 | 1456 / 23 |
| Cyclomatic complexity: sum / maximum | 3015 / 46 | 3001 / 46 |
| Maximum function hotspot score | 265.10 | 189.00 |
| Duplicated source lines | 1320 (6.95%) | 1222 (6.45%) |
| Clone groups | 88 | 89 |
| Escape-hatch occurrences | 27 | 24 |
| Non-test-scoped panic-path findings | 4 | 2 |
| Internal dependency edges | 220 | 214 |
| Mean module leverage | 66.118 | 66.158 |
| Mean module locality | 97.602 | 97.839 |
| Physical Rust lines (`src` + `tests`) | 20539 | 20508 |
| RQLens nonblank source lines | 18983 | 18939 |

Clone group count rose slightly while duplicated lines fell: the measured
improvement is duplicated volume, not fewer groups. Escape reduction comes from
explicit imports in touched modules, not removing unsafe code (none was present).
One remaining non-test-scoped panic finding is actually the `#[cfg(test)]`
notification constructor; the other is diagnostics serialization in `lib.rs`.
This tool does not emit a separate Halstead/effort metric. Hotspot pressure is a
maintenance-risk proxy, not a measured developer-effort reduction.

| Function | Cognitive before → after | Cyclomatic before → after |
| --- | ---: | ---: |
| `PowerEnvelope::reconcile` | 30 → 7 | 38 → 12 |
| `sleep::diagnostics::inspect` | 31 → 15 | 25 → 15 |
| `sleep_policy::hypridle::render` | 30 → 11 | 20 → 13 |

## Design changes

- Separated external power-profile observation, profile application, and durable
  Balanced restoration. Persistence still precedes system effects.
- Flattened optional diagnostics parsing and consumed complete hypridle listener
  blocks, removing the parser's mutable partial-block state.
- Shared PipeWire node/route write transactions, retaining both proxy lifetime
  and the second acknowledgement roundtrip. No unsafe abstraction was added.
- Removed Activity's unused notification sink and the now-unused test adapter.
  Summary publication scans borrowed events and clones only the selected event;
  event sorting no longer clones IDs. Focused state projections avoid copying
  unrelated snapshot domains in notification, Activity, and power paths.
- NotificationService owns native expiry/signal tasks or the fallback monitor.
  DND mutations return their committed state; the notification API no longer
  retains a second StateStore solely to reconstruct that response.
- Unified repeated refresh/error paths without changing reconnect intervals,
  failure-state publication, or event ordering. Removed the audio and display
  planner's unnecessary initialization assertions.

Activity service leverage/locality improved from **48/85 to 57/94**; daemon task
wiring improved from **4.5/49 to 13.5/58**. Notification service leverage fell from
74 to 68.5 as it took ownership of backend lifetime; its locality remains 100.
This is an ownership change, not a forwarding facade. Historical composition-root
baselines in `quality-policy.md` were not silently raised to match today's graph.

## Verification and remaining work

- Formatting, strict all-target Clippy, and RQLens correctness execution pass.
- 152 ordinary tests pass; the isolated PipeWire test also passes (153 total).
  No existing tests were removed. Added summary-selection and transaction-failure
  coverage, and strengthened state-projection/DND response assertions.
- Changed-line coverage against `HEAD`, including the prior audit fixes, is 82%.
  CI now includes the isolated PipeWire test supplied by the dev shell; it uses
  a private socket and virtual devices, not host audio. The 80% gate is unchanged.
- Architecture measurement reports zero violations but remains **partial** due
  to seven unresolved generated Wayland references. This is not an architecture
  gate pass; the existing limitation was not waived or hidden.
- The largest remaining hotspots are battery forecasting, managed lid monitoring,
  and ICS parsing. The API dispatch table still has cyclomatic complexity 46;
  hiding its routes behind macros would not simplify the actual contract.
