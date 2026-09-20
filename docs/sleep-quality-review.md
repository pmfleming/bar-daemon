# RQLens review: sleep reliability follow-up

## Scope and evidence

Compared `fe61dde` with `d8364e9` (sleep refactor) and `7fce70c` (layout-test
fixtures). Measurements cover all discovered Rust sources, including tests,
not just the edited sleep modules. Shelllist source is unchanged.

Used the locally available `../rust-quality-lens/target/debug/rqlens` 0.1.0,
complexity model v2 and architecture risk model v4. Binary SHA-256:
`3e61a97de56766a29ade3fba90fc3583995cea4bbe9c8bc7bac2e93808cf98cb`.
No measurement configuration, scoring rules, exclusions or lint suppressions
were added or changed.

Fresh producers: `hotspots`, `clones`, `leverage`, `locality`, `escape-hatches`,
`reliability`, and `architecture-rules`. Local evidence is retained under
`target/sleep-refactor-before/analysis/` and `target/sleep-refactor-after/analysis/`;
the former also has a sibling source/test snapshot. These are ignored artifacts.
Reproduce each producer from this repository's development shell:

```sh
nix develop . --offline --no-write-lock-file -c \
  ../rust-quality-lens/target/debug/rqlens measure hotspots --config rqlens.toml
```

## Findings and changes

1. **State/effect boundary and split lease ownership.** `StateStore` referenced
   an outcome type in an effects module, violating an existing architecture
   rule. `SleepOperation` now lives in `model.rs`. The tracker atomically owns
   the operation, monotonic start time and FD lease, rather than coordinating
   two additional global/poisonable mutexes and the policy cleanup module.
   Terminal/unknown transitions release the client lease even without a telemetry
   subscriber. Root-helper settling and active-job checks are unchanged.
2. **Preflight and monitor complexity.** Shared lock/session confirmation and
   action capability/dispatch operations replace repeated checks. The lid
   action worker is separate from ownership observation. Critical-battery
   refresh owns evidence, warning delivery and persistence sequencing; its
   monitor only schedules work. Failed warnings still latch before persistence,
   and successful delivery still starts a full grace interval.
3. **Cloning and publication.** Eight whole-snapshot reads were removed or
   replaced by focused projections. Lid/critical updates no longer copy the
   whole sleep-policy state. Publication shares one implementation and retains
   the write-lock/subscription boundary. Runtime override buffers move into
   cleanup ownership; cancellation no longer clones an entire operation.
   Scalar critical-battery policy is `Copy`; FD ownership is not cloned.
4. **Redundant code.** Removed single-use setup/write adapters and a generation
   comparison whose production arguments were always identical. Live Hypridle
   episode validation remains authoritative; setup-race tests now exercise its
   actual predicate. Consolidated duplicated helper-write tests while retaining
   lower/upper bounds, exact contents and failed-write preservation assertions.
5. **Largest clone group.** A layout-test fixture now owns the fake compositor,
   store, temporary document and runtime together. All nine scenarios and their
   assertions remain. Restart tests reset runtime without destroying durable
   recovery state. Explicit imports replace the fixture's wildcard dependency.

## Results and trade-offs

| Whole-project metric | Before | After |
| --- | ---: | ---: |
| Cognitive complexity: sum / maximum | 1604 / 27 | 1590 / 23 |
| Cyclomatic complexity: sum / maximum | 3353 / 48 | 3359 / 48 |
| Mean function cyclomatic complexity | 3.297 | 3.261 |
| Function count | 1017 | 1030 |
| Maximum function hotspot score | 205.97 | 189.00 |
| Duplicated source lines | 1495 (7.07%) | 1415 (6.70%) |
| Clone groups | 111 | 105 |
| Escape-hatch occurrences | 32 | 27 |
| Non-test-scoped panic-path findings | 7 | 2 |
| Mean module leverage | 66.216 | 66.062 |
| Mean module locality | 97.500 | 97.556 |
| Observed internal dependency edges | 238 | 241 |
| Physical Rust lines (`src` + `tests`) | 22823 | 22812 |
| RQLens nonblank source lines | 21151 | 21127 |
| Explicit `.clone()` / `Arc::clone(...)` calls | 313 | 307 |

Not every aggregate improved. Cyclomatic sums include a base of one per
function: additional helpers and regression tests increased that total by six,
while the sum above those bases fell from 2336 to 2329. The unchanged maximum
48 belongs to the API dispatch table; hiding routes behind macros would not
simplify its contract.

State locality improved **79 → 81.25**, model locality **71.5 → 73.75**.
Display-policy/layout leverage improved **66 → 68.5** / **56 → 58.5**.
Mean leverage nevertheless fell: effects modules lost inappropriate data-type
consumers, and explicit fixture imports exposed five dependencies that the old
wildcard import had not recorded. Those fixture dependencies already existed
at runtime. This is not evidence that all architecture pressure decreased;
no forwarding module or weaker identity policy was introduced to improve scores.

| Hotspot | Cognitive before → after | Cyclomatic before → after |
| --- | ---: | ---: |
| Sleep preflight/dispatch orchestration | 18 → 6 | 36 → 23 |
| Connected lid observer | 14 → 11 | 31 → 25 |
| Critical-battery monitor | 27 → 7 | 18 → 9 |
| Sleep-policy save | 10 → 8 | 23 → 19 |

RQLens does **not** emit a separate Halstead/effort metric. Hotspot reduction
is a maintenance-pressure proxy, not measured developer-hours saved. Clone-call
counts also do not distinguish cheap reference counting from heap copies.
Escape reduction here is explicit imports, not removal of necessary safety
contracts. The two remaining non-test-scoped panic findings are diagnostics
serialization and a notification constructor that is actually `#[cfg(test)]`.

## Verification and remaining review

- Formatting, strict all-target Clippy, build and cargo-shear pass.
- 177 ordinary unit tests and two startup tests pass. One unrelated PipeWire
  unit test remains ignored by default. The isolated native Hypridle test was
  explicitly run and passed: **180 passing tests** in total.
- New regression coverage exercises terminal lease release without a monitor,
  stale cancellation guards, delayed warning delivery and failed-warning latching.
- Cross-repository API fixture/registry and battery/bar JavaScript checks pass.
  Rust doctest execution succeeds (there are no doctests).
- Observed architecture violations: **1 → 0**. Evidence is still **partial**:
  the same 14 macro-generated Wayland references remain unresolved in lock
  observation and native-idle test code. This is not a complete architecture
  gate pass, and other previously generated/stale artifacts were not used.
- No live power-policy changes, sleep/hibernate, service activation or remote
  pushes were performed.

Remaining prominent hotspots are battery forecasting (cognitive 20,
cyclomatic 24), ICS parsing (cognitive 23), and layout recovery. They are
separate follow-up candidates, not silently moved or excluded from these totals.
