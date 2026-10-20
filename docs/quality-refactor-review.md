# RQLens review: location and timezone refactoring

## Scope and findings

Measured the project with the local `rust-quality-lens` checkout (RQLens 0.1.0,
Rust 1.95). The largest production cognitive hotspots were the location-authority
loop and timezone monitor. The related code also duplicated timezone application,
network/control validity checks, and tzdata alias parsing.

Changes:

- **Authority:** separate control reconciliation from network observation; share
  cancellation predicates, revision-aware status publication, and verified timezone
  application. Durable intent, final cancellation checks, readback, and uncertain
  outcome handling remain in their original order.
- **Timezone reader:** isolate signal decoding and revision-consistent location
  reads; reuse the bounded location D-Bus call instead of another proxy/timeout
  implementation. Legacy services without `GetControl` still work.
- **Ownership/locality:** keep Home and catalog matching policy in
  `activity::locations`, not in the privileged binary's shared validation module.
  Share `TimezoneLinks` between both consumers and parse the session alias table
  once, rather than rebuilding a map and allocating canonical strings per match.
  These are module functions, not cross-file implementations of `Place`.
- **Cloning/safety:** borrow catalog data instead of cloning the whole catalog
  before add/remove/forecast configuration; move the accepted manual place;
  copy scalar controls. Remove two production `unwrap()` calls and the binary's
  blanket `allow(dead_code)` without adding suppressions.
- **Tests:** share restart/shutdown setup without removing scenarios. Extend
  alias-chain/cycle/malformed-table checks and stable/torn revision checks.
  Replace a flaky two-second wall-clock assumption with an exact assertion that
  the persisted legacy cooldown deadline is unchanged.

## Measured results

Same tool, configuration, and source scope on both sides; output directories differ.
The function metrics include newly extracted helpers in the project totals.

| Function | Cognitive | Cyclomatic | Halstead effort |
| --- | ---: | ---: | ---: |
| `Authority::run` | 53 → 10 | 34 → 11 | 215,390 → 34,662 |
| `timezone::monitor_connection` | 27 → 16 | 22 → 17 | 209,852 → 156,115 |
| `Authority::apply_control` | 17 → 12 | 26 → 21 | 251,830 → 166,091 |
| `Authority::check` | 10 → 9 | 30 → 23 | 253,044 → 201,351 |
| `Authority::acquire` | 11 → 10 | 20 → 14 | 161,082 → 144,987 |
| `locations::append_current` | 14 → 9 | 12 → 11 | 98,327 → 92,178 |

| Project measurement | Before | After |
| --- | ---: | ---: |
| Sum of function cognitive complexity | 2,760 | 2,711 |
| Sum of function cyclomatic complexity | 5,637 | 5,628 |
| Sum of function Halstead effort | 26,295,529 | 25,933,816 |
| Duplicated lines | 1,609 | 1,497 |
| Duplication percentage | 4.37% | 4.07% |
| Escape-hatch sites (sum of category counts) | 40 | 39 |
| Reliability findings, including tests | 1,005 | 993 |
| RQLens source lines | 36,804 | 36,798 |
| Mean module locality score | 96.54 | 96.54 |
| Mean module leverage score | 27.79 | 27.86 |
| Shared place/alias module leverage | 90 | 100 |

Five explicit `.clone()` calls were removed from the changed files, including
three whole-catalog clones. The two removed production panic sites are in
`location/control.rs`; the remaining reliability-count reduction is in tests.

**Limits:** locality is better organized by responsibility, but the numeric
project locality score is unchanged; the principal location modules were already
at 100. LOC reduction is small: physical Rust line count actually increases by
10 with spacing/comments and regression assertions. RQLens reports six fewer
source lines and 18 fewer function lines. This is not a large code-pruning pass.

All artifacts retain partial-confidence flags for authored-syntax macro/ownership
limitations. These are heuristic/static comparisons, not proof of complete
compiler inventory, defect probability, coverage, or runtime performance.

## Validation

- Private fixtures: **196 passed, zero failed or ignored**, including PipeWire,
  native Hypridle/Wayland, and location D-Bus fixtures.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --doc --locked`: passed (zero doctests).
- All 13 changed Rust files pass rustfmt; `git diff --check` passes.
- Repository-wide `cargo fmt --all -- --check` still reports the same pre-existing
  differences in `src/activity/mod.rs`, `src/api.rs`, and `src/lib.rs`.

Existing edits to `flake.nix` and `docs/location-timezone.md` were not changed.

## Evidence and reproduction

Local evidence is under `target/quality-refactor-current/`:

- `before/`, `after/`: hotspots, clones, escape hatches, reliability, locality,
  and leverage JSON, with matching fingerprints within each measurement set.
- `before.toml`, `after.toml`: configuration snapshots.
- `comparison.txt`, `compare.py`: summary and comparison script.
- `fixtures-verified/complete.txt`, `fixtures-verified/tests.log`:
  successful full-fixture evidence.
- `clippy-final.log`, `doctests.log`, `fmt-final.log`: verification output.

```sh
RQLENS=../rust-quality-lens/target/campaign-tool-build/debug/rqlens
for metric in hotspots clones escape-hatches reliability locality leverage; do
  "$RQLENS" measure "$metric" --config target/quality-refactor-current/after.toml
done
bash tools/quality-fixtures.sh
cargo clippy --locked --all-targets -- -D warnings
cargo test --doc --locked
```

Next review candidates, not changed here: GeoClue acquisition (`locate_on`,
cognitive 17 / cyclomatic 38), lid monitoring (`connected`, 16 / 28), and
notification expiry (`EngineData::expire`, 16 / 12). Keep their concurrency and
safety contracts explicit rather than merely moving branches to lower scores.
