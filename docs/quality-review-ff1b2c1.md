# RQLens-guided borrowing and locality review

Baseline: bar-daemon `ff1b2c1`; analyzer:
`../rust-quality-lens/target/debug/rqlens`, revision `2910143`, Rust 1.95.
The requested local analyzer checkout is named `rust-quality-lens`.

## Findings and changes

- **Display safety was duplicated across planning and execution.**
  `Output::has_active_mirrors` now serves both paths; `Plan::validate_disable`
  owns topology revalidation and keeps its external signature private.
  Reconciliation still checks eligibility, rereads topology before each disable,
  and checks sleep interruption before applying an effect. Plans borrow their
  target outputs instead of cloning complete mode lists.
- **Snapshot types pulled effectful monitors into the data model.** Moved
  compositor/work-area DTOs into `src/model.rs`, removing those model-to-monitor
  dependencies. Notification snapshot DTOs now live with the other notification
  types in `activity/notifications/model.rs`; the engine and SwayNC no longer
  depend on the global model. No forwarding module or compatibility re-export
  hides these dependencies. Wire formats are unchanged.
- **Notification history duplicated transport and decoding work.** Query/list
  requests share a typed request/reply helper and row decoder. The decoder reads
  SQLite's borrowed text directly instead of allocating a payload `String`.
  Errors cross the worker boundary without a String-to-error round trip.
  Page-size checks still precede decoding; queue reservations, mutation order,
  sticky history failures, exclusion filtering and pagination limits remain.
- **Unnecessary copies and unused code remained.** Removed the unused
  `HistoryNotification -> CatalogRecord` conversion, unused `Clone` derives,
  and test-only alternate database reader. Persistence tests now exercise the
  production reader. Notification ingress borrows its semaphore instead of
  cloning an `Arc`; action parsing consumes owned strings. History cursor
  construction no longer needs `expect`.
- Display output serialization uses `collect_seq` and borrowed mirror names,
  rather than materializing a second output vector and cloning names.
  Weather configuration borrows the configured locations; the legacy single
  location still gets an owned, normalized copy. Calendar display-name fallback
  is shared by the ICS provider and activity service. SwayNC disconnection reads
  only notification state, not a clone of the entire bar snapshot.
- Logind property subscriptions share the same object-based setup in lid and
  sleep monitoring. Removed three test glob imports; generated Wayland lint
  allowances and required protocol signatures were not weakened.

## Measurements

Function sums include tests and extracted helpers, but not module aggregate rows.
The same 89 modules are measured before and after. RQLens's `clones` producer
measures duplicate source, **not Rust allocation or `Clone` calls**.

| Observation | Before | After |
| --- | ---: | ---: |
| Cognitive complexity sum | 1,806 | 1,795 |
| Cyclomatic complexity sum | 3,728 | 3,718 |
| Function hotspot-score sum | 15,968.37 | 15,866.01 |
| Duplicate nonblank lines | 1,464 | 1,445 |
| Duplication percentage | 6.12% | 6.05% |
| Escape-hatch occurrences | 26 | 23 |
| Physical Rust lines (`src/` + `tests/`) | 25,722 | 25,700 |
| Reliability findings, including tests | 553 | 553 |
| Mean observed module leverage | 24.83 | 24.94 |
| Mean module locality | 97.61 | 97.61 |

`display_policy::reconcile`: cognitive 12 → 8, cyclomatic 17 → 13,
pressure 121.24 → 77.90. Some decisions now belong to `Plan::validate_disable`;
the function sums above include that helper.
`Planner::plan`: cyclomatic 12 → 11, pressure 103.54 → 96.88.
`lid::connected`: cyclomatic 25 → 23, pressure 161.95 → 150.86.

### Locality and leverage trade-offs

Global model locality improves **72.25 → 77.50**, and notification-engine
locality improves **88 → 91**. State-store locality falls **74.50 → 67**:
its explicit notification-model import exposes additional parent-module edges,
and explicit test imports expose previously uncounted state consumers. Activity
locality falls 99.25 → 98.50. **Aggregate locality is unchanged**, not a broad
score improvement. The benefit is better ownership and fewer model-to-effect
cycles, with the remaining state-store coupling visible.

Leverage's small aggregate gain includes improved visibility from explicit test
imports, and is partly offset by removing unwanted monitor consumers. Shared
row decoding, calendar naming, mirror checks and logind subscriptions are real
function reuse; the module score does not directly measure that reuse.
No composition-root baseline was raised. Existing daemon/task fan-out already
exceeds the older documented baseline and still warrants separate review.

RQLens provides no Halstead or developer-effort measurement. Hotspot pressure is
only a maintenance-effort proxy, not elapsed developer or CPU time. Line savings
are modest; regression coverage was retained and weather borrowing/legacy
normalization assertions were added. Allocation reductions are structural, not
benchmarked. The removed production `expect` is offset in the total reliability
count by an added test `unwrap`; source-scope classification also mislabels the
`#[cfg(test)]` notification-engine constructor as production.

## Verification and limitations

Passed:

- `cargo fmt --package bar-daemon -- --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --all-targets -- --include-ignored`: **163 passed**, none
  ignored, including PipeWire, native idle, protocol, persistence, display-safety
  and sleep-monitor fixtures.
- `cargo test --locked --doc` (no doctests)
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps`
- `cargo shear` and `git diff --check`

Fresh RQLens producers: `hotspots`, `clones`, `escape-hatches`, `reliability`,
`locality`, `leverage`, `architecture-rules`, each with `--config rqlens.toml`.
Local baseline artifacts are in `target/refactor-baseline/`; current results are
in `target/analysis/`. Unrerun artifacts there are not fresh verification or
coverage evidence.

All **913/913** current dependency references resolve (baseline 888/888), with
**zero violations** of the three architecture rules. Nevertheless,
**`rqlens check` fails**: architecture evidence remains partial because of
unexpanded state/Wayland macros and block-local implementation inventory gaps.
The baseline has the same limitation. No waiver was introduced.

The neighboring `daemon-framework` checkout was being edited concurrently.
One measurement rejected changing inputs and was rerun successfully. A full
`cargo fmt --all -- --check` also encountered formatting changes in that dependency;
package-scoped formatting passes. These are not hermetic dependency-pinned build
comparisons. Coverage, mutation testing, allocation benchmarks and Rust 1.85 MSRV
compatibility were not verified.

Remaining hotspots include notification database migration, lid connection
orchestration and the transport routing match. Generated-code evidence gaps,
state-store fan-out and the remaining serialization invariants need separate
review; this pass does not claim the entire project is free of dead code or panic
paths.
