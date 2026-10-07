# Media ownership and fixture reuse review

Baseline: `3f18c38`. Measured with the local `../rust-quality-lens/target/debug/rqlens`
(checkout `c6898928`), Rust/Cargo 1.95 and rust-analyzer 2026-06-01. Both measurements
use unchanged `rqlens.toml`; no thresholds, waivers or suppressions were added.
Local artifacts and command logs are in `target/refactor-review/{before,after}`
and `target/refactor-review/*.log` (not committed).

## Findings and changes

- **Enrichment mixed observation, scheduling and cache policy.** Separate per-player
  preparation from batch admission; share bounded cache insertion between completion
  and worker failure. `HashMap::entry` and an explicit enabled fetcher remove two
  production `expect` calls. Keep generation/owner checks, negative TTLs, request
  budgets and cancellation semantics.
- **Observation copied content on every MPRIS signal.** Compare borrowed content keys
  and retain a raw player snapshot only when content changes. This retains more fields
  per session, but avoids repeatedly copying title, artist, album, artwork and source
  strings for timing/capability updates. Cache and sessions share `Arc<Metadata>`;
  the artwork file no longer needs a second Arc. MPRIS strings/artist arrays are read
  by reference; thumbnail URLs move rather than clone. These are ownership changes,
  not benchmarked performance claims.
- **URL policies were duplicated.** Share lexical validation and the Audible regional
  domain list in `media::source`, retaining caller-specific host/scheme/path restrictions.
  Reuse the parsed URL for YouTube ID extraction and stop allocating a query-pair
  vector. Preserve the literal-scheme check: a repaired URL containing `://` only in
  its query must still be rejected. Existing malformed-URL tests caught an attempted
  over-simplification of that check during this pass.
- **Provider code knew about MPRIS publication.** Move metadata application into the
  enrichment coordinator. The YouTube transport returns provider data and no longer
  imports the global player model. Split PWA flag extraction from identity validation;
  share thumbnail filename validation between JPEG and WebP without widening the allowlist.
- **Private D-Bus fixture setup was repeated six times.** `src/test_support.rs` owns
  socket pairing and concurrent handshakes; each caller still owns its interfaces and
  server lifetime. Move MPRIS tests to `src/media/tests.rs`, keeping fixture dependencies
  out of the observer implementation. No tests or assertions were removed.
- **Small redundant code remained.** Remove the owned `Content` type, unused Clone/Default
  implementations and unnecessary selector visibility. Battery energy bounds now use
  one pass; adjacent pairs no longer have an impossible slice-length fallback. UPower
  moves its native path after reading cycles and uses the existing state defaults.

## Comparable measurements

Raw complexity and Halstead values below are advisory, not time estimates or verified
correctness. Halstead totals are **sums of per-function effort**, not whole-file effort.
“Non-test” excludes `tests/`, `*tests.rs`, `tests::` qualified names and the test-support
module. All-source rows retain tests and the new files.

| Metric | Before | After |
| --- | ---: | ---: |
| Non-test cognitive sum | 1,637 | 1,625 |
| Non-test cyclomatic sum | 3,471 | 3,469 |
| Non-test Halstead effort sum | 10,158,552 | 10,036,996 |
| All-function cognitive sum | 2,008 | 1,999 |
| All-function cyclomatic sum | 4,086 | 4,095 |
| All-function Halstead effort sum | 16,803,001 | 16,657,569 |
| RQLens source SLOC | 26,370 | 26,352 |
| Physical Rust lines in `src/` + `tests/`, including new files | 28,395 | 28,392 |
| Duplicated lines | 1,598 (6.06%) | 1,461 (5.54%) |
| Glob imports | 27 | 23 |
| `unwrap` / `expect` findings, including tests | 629 / 14 | 609 / 11 |
| Mean observed-reuse leverage | 25.00 | 25.59 |
| Mean locality | 97.39 | 97.43 |

| Function | Cognitive | Cyclomatic | Halstead effort |
| --- | ---: | ---: | ---: |
| `Enrichment::prepare` | 23 → 8 | 16 → 7 | 145,137 → 48,946 |
| PWA `parse` | 19 → 7 | 34 → 14 | 80,139 → 20,772 |
| `parse_source` | 16 → 13 | 24 → 16 | 40,433 → 27,200 |
| `thumbnail_url` | 7 → 4 | 19 → 16 | 31,530 → 23,524 |

Extraction does not erase decisions: new `prepare_player` is cognitive/cyclomatic 10/8,
and `launch_flags` is 10/18. Expanded tests and fallible fixture setup increase the
all-function cyclomatic sum. Total LOC reduction is deliberately modest rather than
obtained by dropping tests. Module means also change with test-module boundaries and
explicit imports: locality is essentially stable, not a major architectural improvement.
The global model's locality declines from 75.25 to 74.50 with the separately inventoried
media tests. No production composition-root fan-out was hidden behind forwarding modules.

## Verification and limits

Passed:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --include-ignored
cargo test --locked --doc
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
```

All **191 unit tests and two integration tests** pass, including private PipeWire and
native-idle fixtures. Extended cases cover PWA missing/empty flags, control characters,
all supported thumbnail sizes/formats and malformed paths, content invalidation, and
preservation of pending tickets across timing/capability updates. No live YouTube or
host sleep actions are needed by these tests.

Reproduce the focused producers with:

```sh
for metric in hotspots clones escape-hatches reliability leverage locality architecture-rules; do
  ../rust-quality-lens/target/debug/rqlens measure "$metric" --config rqlens.toml
done
```

All three architecture rules report **zero observed violations**, and all 1,106 dependency
references resolve. Nevertheless, **`rqlens check` exits 1**: architecture evidence remains
partial because the analyzer does not fully inventory existing macros and block-local
implementations. The baseline has the same limitation. Unrefreshed coverage/practice/etc.
artifacts are also stale; this focused pass is not a successful full `measure all` or
policy certification. Coverage, mutation testing, dependency audits and Rust 1.85 MSRV
compatibility were not revalidated.

## Remaining priorities

- `work_area::monitor_with` (cognitive 18) and notification `persistence_worker` (17)
  remain event-loop hotspots. Preserve cancellation/recovery tests before restructuring.
- `battery::derived::energy` still has high per-function effort (152,462); numerical
  boundary coverage should precede a larger algorithm split.
- The flat API dispatcher has cyclomatic 50 but low nesting. Do not replace explicit
  routing with indirection solely to lower that number.
- Existing composition-root fan-out (`daemon::tasks` 21, `api::effects` 15) exceeds older
  documentation baselines in places. This pass does not raise those baselines or claim
  to resolve that architectural review. Clippy reports no dead-code warnings; no larger
  unused subsystem was identified for safe deletion.
