# Five follow-up quality improvements

Baseline: clean `e752dae6c04e8b1c9fa9dba054bd8be99e6e013f`, including the newer
Activity location and calendar-boundary changes. Those production behaviors are
unchanged. RQLens: clean local `../rust-quality-lens` commit
`b997a692242690cfb6bf7cab9f8296ec097497e8`, using one frozen binary with SHA-256
`09b72768d645f97886ae4b22ced05553bbcac988fac290745e66f97405e94053`.
Evidence/logs/configs: `target/quality-next-five/`. Source scope: 110 files;
before/after fingerprints: `79d4ddcd6d3483d1` / `6a76fd7b6d621411`.
These measurements were captured before splitting the work into five commits.
The final Rust sources are identical; committing changes Git provenance, so
regenerate evidence before checking freshness against a later HEAD.

## Changes

1. **Media URL parsing — `src/media/source.rs`.** Separate URL acceptance from
   exact service classification. YouTube enrichment no longer extracts and
   allocates the same ID twice through content inference. IDs borrow URL text
   where possible; only the returned enrichment ID must become owned. A bounded
   three-slot path projection replaces temporary segment vectors. Credential,
   authority, port, ID, duplicate-query, path-arity, and scheme restrictions are
   preserved. Tests cover decoded IDs, trailing slashes, extra segments, and
   rejection of a short-link host carrying an embed path.
2. **PipeWire decoding — `src/audio.rs`.** Share POD object decoding between node
   properties and routes. Match property key and value type together instead of
   repeating guards. Wrong types still cannot supply required route fields;
   empty channel arrays retain scalar-volume fallback. Tests exercise real POD
   serialization, defaults, channel-volume precedence, wrong types, and non-object
   input, in addition to the private PipeWire fixture.
3. **Notification expiry — `src/activity/notifications/engine.rs`.** Consume due
   one-shot deadlines with `Option::take_if` and collect expired IDs during the
   existing active-notification traversal. Future snoozes still block both popup
   and content expiry; popup-only changes still do not dirty history. Explicit
   deadline-boundary tests verify wake/close batching, policy and DND transitions,
   disabled policies, and idempotent repeat ticks. Notification cloning remains
   limited to wakeups that require persistence.
4. **Effective sleep configuration — `src/sleep_policy/runtime.rs`.** Match exact
   section/key assignments instead of nested comment/section/key branches. Bare
   keys and comments remain ignored; empty values and later assignments retain
   reset/override semantics. Extend tests for comments, whitespace, section
   switching, bare keys, and assignments outside `[Sleep]`.
5. **Swap evidence — `src/sleep/diagnostics.rs`.** Validate the fixed five-column
   record shape without allocating a vector per row. Accumulate disk availability
   without stopping validation after the first disk: malformed later rows still
   make the evidence unknown. Tests cover missing/extra columns, bad types/sizes,
   zero-sized swap, zram naming, and malformed trailing records.

Validation also exposed a pre-existing strict-Clippy failure in the newer
`src/activity/locations.rs` test. Initialize that fixture directly and replace
its wildcard import with explicit names. No production location logic or test
assertion changed, and no lint suppression was added.

## Measurements and tradeoffs

Production aggregate below includes all function rows in the five edited
production modules, excluding inline test modules and the engine's two
`cfg(test)` methods. New helpers are included (95 functions before, 99 after).
Halstead effort is a syntax heuristic, not developer hours or benchmark evidence.

| Metric | Before | After |
| --- | ---: | ---: |
| Edited production cognitive sum | 298 | 270 |
| Edited production cyclomatic sum | 476 | 468 |
| Edited production Halstead effort | 1,641,297 | 1,593,569 |
| All-function cognitive sum, including tests | 2,211 | 2,188 |
| All-function cyclomatic sum, including tests | 4,516 | 4,517 |
| All-function Halstead effort, including tests | 19,069,522 | 19,226,480 |
| Duplicated lines / groups | 1,620 / 127 | 1,606 / 126 |
| Escape-hatch occurrences | 28 | 27 |
| Mean locality | 97.320 | 97.320 |
| Mean observed-reuse leverage | 26.545 | 26.545 |
| Rust physical lines, `src` + `tests` | 31,730 | 31,914 |

Production code shrank by nine lines; new/extended regression tests and fixture
hygiene add 193. **Total Rust LOC and all-function effort increased**, rather than
pretending tests are free. Four tests were added; none deleted or newly ignored.
There is no measured aggregate leverage/locality improvement. Maximum cognitive
and cyclomatic complexity remain 21 and 41 in untouched functions.

Selected cognitive/cyclomatic changes: PipeWire `parse_route` **7/16 → 3/12**,
`parse_props_object` **9/10 → 6/7**, sleep `verify_effective` **13/10 → 5/6**,
and swap parsing **9/11 → 4/8**. Expiry's cognitive score stays 16, while its
cyclomatic score falls 13 → 12 and effort falls 43,750 → 31,230. Swap parsing's
fixed-shape pattern increases its own Halstead effort despite removing an
allocation and reducing branches. No allocation-rate or throughput benchmark was
run, and no result is claimed for dead-code elimination beyond removed redundant
parsing/traversal paths.

## Validation

- **235 tests pass, zero ignored**: 233 unit tests and two integration tests,
  including the isolated PipeWire and native Hypridle fixtures.
- Strict all-target Clippy, formatting, and diff whitespace checks pass.
- `rqlens verify`: **20 passed, eight optional checks skipped**, zero unavailable
  checks or diagnostics. Includes configured dependency checks, doctests and
  rustdoc. No new MSRV toolchain execution was performed.
- Full-fixture changed-line coverage: **100% (283/283)** against baseline HEAD,
  including executable test lines. Line coverage is not exhaustive branch or
  property coverage.
- All **1,259 collected dependency references resolve**; no architecture-rule
  violation was observed. Nevertheless **`rqlens check` still fails** because the
  existing macro/lexical-impl inventory is partial. There were no stale artifacts
  at measurement time. Focused measurement does not produce all unrelated
  evidence artifacts.
- The two existing production-scoped panic advisories remain unchanged.

### Sequential commit validation

Each improvement was isolated and validated without subsequent changes present.
Formatting, strict all-target Clippy, whitespace checks and the full fixture suite
passed before each commit. Logs are in `target/quality-next-five/commit-series/`.

| Step | Improvement | Passing tests, including integration tests |
| --- | --- | ---: |
| 1 | Media URLs; accompanying test-only Clippy cleanup | 232 |
| 2 | PipeWire decoding | 233 |
| 3 | Notification expiry | 234 |
| 4 | Effective sleep configuration | 234 |
| 5 | Swap evidence; aggregate review report | 235 |

Reproduction (inside the development shell):

```sh
for metric in hotspots clones escape-hatches reliability locality leverage architecture-rules; do
  target/quality-next-five/rqlens measure "$metric" --config rqlens.toml
done
target/quality-next-five/rqlens verify --config rqlens.toml
cargo clippy --workspace --all-targets --locked -- -D warnings
QUALITY_OUTPUT_DIR=target/quality-next-five-recheck bash tools/quality-fixtures.sh coverage
diff-cover target/quality-next-five-recheck/cobertura.xml --compare-branch e752dae --fail-under 80
# Completeness still blocks, even with no observed architecture violations:
target/quality-next-five/rqlens check --config rqlens.toml
```
