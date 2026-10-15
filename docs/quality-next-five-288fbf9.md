# Five quality follow-ups

Baseline: `288fbf9` (the previous, already validated refactor was checkpointed
separately). Each follow-up was implemented, tested and committed before starting
the next. Local evidence: `target/quality-next-five-current/`.

## Changes and per-step validation

1. **Audio contract ownership** (`000aaef`): move declarations into
   `protocol/audio_wire.rs`, with an effect-handler trait implemented by the API.
   Protocol envelopes now belong to protocol rather than transport. Remove unused
   API metadata re-exports and make its audio implementation module private.
   Extend the architecture rule to cover the new wire module. Registry and control
   fixture JSON are byte-for-byte unchanged; 15 protocol/API tests and strict
   all-target Clippy pass. Observed architecture violations fall from one to zero.
2. **Weather cache ownership** (`d2f7695`): move cached forecasts through refresh
   and failure fallback instead of cloning their hourly/daily buffers again.
   Preserve the live snapshot while fetching; fresh entries do not poll HTTP.
   Boundary, buffer-reuse, unavailable-entry and failure tests pass, as do all
   51 activity tests. The two production refresh functions together reduce
   cognitive complexity 6 → 4 and effort 55,497 → 39,612; cyclomatic stays 7.
3. **Calendar interval invariants** (`b17b6b7`): centralize validated half-open
   intervals, local date boundaries and membership in `day::QueryRange`.
   ICS loading and service queries share deterministic event ordering. All 52
   activity tests pass, including DST, exclusive ends, instant events and todo
   timestamp precedence. Across all four changed files, including tests,
   cognitive complexity falls 92 → 87 and effort 1,098,829 → 1,040,116;
   cyclomatic increases 158 → 161.
4. **Wayland backpressure** (`2941a4f`): isolate flushing without moving it outside
   the roundtrip deadline. Isolated tests exercise real socket write backpressure,
   silent peers, lock/unlock transitions and disconnection; no desktop is used.
   Remove a glob import. Both tests and strict Clippy pass. Combined production
   cognitive complexity falls 16 → 14 and effort 34,484 → 27,183; cyclomatic
   increases 16 → 18 because the new function and fallible call add paths.
5. **Ordered sleep diagnostics** (this commit): replace nested message emission
   with ordered evidence rules, sharing model normalization and message filtering.
   Preserve missing/malformed evidence as unknown and keep resume guidance separate
   from hibernation issues. Extend tests for combined issue ordering and guidance
   counts. All 16 sleep tests and strict Clippy pass. Combined builder/filter
   cognitive complexity falls 15 → 8, cyclomatic 15 → 11, effort 76,018 → 61,226.

## Aggregate results and trade-offs

Function totals cover all changed Rust files, including the new audio wire module.
The non-test row excludes `tests`/`*_tests` modules, not newly extracted helpers.

| Function scope | Cognitive before → after | Cyclomatic before → after | Effort before → after |
| --- | ---: | ---: | ---: |
| Non-test functions | 151 → 134 | 327 → 326 | 1,966,419 → 1,851,291 |
| Including tests | 211 → 199 | 395 → 401 | 2,621,695 → 2,617,356 |

Tracked Rust lines increase **36,811 → 37,040**: explicit contracts, interval
representation and regression tests cost code. Literal `.clone()` calls decrease
599 → 598, escape occurrences 40 → 39; duplicated lines increase 1,546 → 1,563.
These are not universal metric improvements, and no tests were deleted to obtain
better counts. No compiler dead-code or unused-dependency findings remain.

API locality improves 59.5 → 65.5, but protocol-root locality falls 60.25 → 50.5
as shared envelopes move its fan-in there. The new audio wire module has locality
100 and leverage 40; audio implementation leverage falls 30 → 10 as consumers
now use the contract. A dedicated envelope owner is a possible further refinement;
no pass-through modules were introduced just to inflate scores.

## Final verification and limits

- Full fixture runner: **219 unit + two integration tests pass**, zero ignored,
  including private PipeWire and native-idle tests.
- Strict all-target Clippy after every step; final package formatting, doctests
  (zero defined), `cargo shear`, and `git diff --check` pass.
- RQLens: per-step hotspots; final hotspots, clones, escape-hatches,
  architecture-rules, locality and leverage. Final architecture has three rules,
  zero observed violations and zero unresolved collected references.
- Concurrent local-dependency edits invalidated the first live architecture run.
  Architecture/final measurements use frozen source and dependency copies in
  `target/quality-next-five-current/frozen/`, with the same pinned analyzer.
- Macro/generated inventory remains partial. No full strict-policy, coverage,
  mutation or audit certification is claimed.
