# RQLens review — 2026-10-23

Baseline: `66cebe927f5046fd651fd379dda43e618205d186`, including the pre-existing
module-order change in `src/activity/notifications/mod.rs` (preserved).
Analyzer: local `../rust-quality-lens/target/debug/rqlens`, copied before use;
SHA-256 `606229744191af27c6c3652691f61984b9ba30c3a942c5968d9b6b4f15bcc9f2`.

## Findings and refactoring

- **Notification action extraction:** the largest production cognitive hotspot
  combined URL admission, meeting classification, ambiguity detection, and
  clipboard inference. Parse each URL once; share an allocation-free uniqueness
  check across links, codes, and phone numbers. Keep exact host/path checks,
  credential rejection, leading zeros, and conservative code/link handling.
  Extraction now accepts text rather than depending on the notification model.
- **Notification ownership:** select only the owned action target under the
  existing version/expiry fence, then release locks before external effects.
  Avoid cloning an entire notification and allocating descriptors just to compare
  action keys. Move replacement monitor identity in the model, rather than copy it
  out and restore it in the engine. Summary publication copies monitor names, not
  the complete workspace snapshot, and computes the fallback once.
- **Toast decks:** reuse frozen records in place instead of copying old records
  and then cloning the entire deck only to replace its record vector. Retain
  acknowledged-removal compaction and inert replacements during reading leases.
- **Protocol ownership and reuse:** move request decoding from transport into the
  existing protocol module. Wire constructors and fixtures depend directly on
  their protocol owner, not API re-exports. Share typed success serialization
  across nine consumers without reserializing existing JSON replies. Remove
  forwarding envelope methods, the redundant control failure enum, and the old
  audio reply serializer/variant; audio fixtures now use the live audio DTO.
  The retired audio serializer was fixture/test-only, not compiler-proven dead
  code. Its controller test now checks the raw readback error and state preservation.
- Replace two test glob imports with explicit imports. No new suppressions,
  dependencies, macros, unsafe code, or policy exceptions. `src/media.rs` has only
  a rustfmt correction. Compiler checks and cargo-shear found no additional dead
  code or unused dependencies to remove safely.

## Measurements

Evidence: `target/quality-review-current/snapshot/{before,after}/analysis`.
Commands: `measure hotspots`, `clones`, `escape-hatches`, `reliability`, `locality`,
`leverage`, and `architecture-rules`, using each snapshot's `rqlens.toml`.

Complexity totals cover **all functions in changed Rust files**, including added
helpers, removed functions, contract generators, and tests—not just smaller entry
functions. Halstead effort is a syntax heuristic, not developer hours.

| Signal | Before | After |
| --- | ---: | ---: |
| Cognitive complexity sum | 404 | 398 |
| Cyclomatic complexity sum | 793 | 783 |
| Halstead effort sum, rounded, including tests | 5,314,248 | 5,320,849 |
| Cognitive sum outside top-level test modules | 326 | 319 |
| Cyclomatic sum outside top-level test modules | 637 | 627 |
| Effort outside top-level test modules | 3,897,120 | 3,868,154 |
| `context_actions::extract` cognitive / cyclomatic | 21 / 28 | 8 / 6 |
| `context_actions::extract` effort | 102,807 | 33,162 |
| Project duplicated lines | 1,623 | 1,602 |
| Project escape-hatch occurrences | 41 | 39 |
| Project reliability finding sites | 1,138 | 1,125 |
| Literal `.clone()` calls, `src/` + `tests/` | 659 | 652 |
| Physical Rust lines, `src/` + `tests/` | 42,979 | 42,945 |

Expanded target-extraction and frozen-deck assertions increase test effort; the
all-function effort total is **0.12% higher**, despite lower non-test-module
effort. The shared serializer retains the existing infallible-DTO expectation;
reducing reported panic sites does not make serialization universally infallible.

Architecture trade-offs are explicit:

- API inbound dependencies fall **14 → 5**; locality risk **43.5 → 42**.
- Battery and notification API locality risks each fall **15 → 12**; battery and
  query fixture risks each fall **12 → 9**.
- Protocol observed reach rises **33 → 38**, with its leverage score already
  saturated at 100. Protocol locality risk rises **48.75 → 52.5** as reuse moves
  to its proper owner. API leverage falls **100 → 50**, appropriately removing
  consumers of a transport composition root. No across-the-board leverage-score
  improvement is claimed or manufactured with artificial consumers.
- Summed module locality risk falls **617.25 → 606.75** (an advisory comparison,
  not a calibrated project score). All three configured architecture rules report
  zero violations and zero unresolved collected dependency references.

## Validation and limits

- Before and after: `tools/quality-fixtures.sh test` passes **223 unit tests and
  two integration tests**, including private PipeWire/native-idle fixtures, with
  zero ignored tests. Final evidence: `target/quality-fixtures.6MihaY`.
- Final formatting, strict all-target Clippy, doctests, cargo-shear, and
  `git diff --check` pass. Existing live/fixture contract tests remain passing.
- Extended tests cover duplicate/ambiguous targets, normalized URLs, lookalike
  meeting hosts, credentials, URL length, code/link exclusion, phone limits,
  leading zeros, repeated frozen projection, removal, and replacement monitor
  preservation. No desktop-facing effect was invoked for validation.
- Neighboring `daemon-framework` and `update-daemon` checkouts changed during
  analysis. RQLens correctly rejected two live architecture runs. The reported
  comparison uses frozen before/after project sources against **identical frozen
  dependency inputs**. Those neighboring checkouts were not modified or reverted.
- Semantic resolution covers 100% of collected references, but macro/impl
  inventory remains explicitly partial. This is not strict quality certification;
  no new coverage, mutation, audit, or production-performance claim is made.

Remaining hotspots include update-history pagination, update observation, and
location acquisition. They were reviewed but left outside this bounded pass;
safety-sensitive state machines were not split merely to lower function scores.
