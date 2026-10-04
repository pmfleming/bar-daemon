# RQLens-guided ownership and duplication review

Baseline: bar-daemon `1e4e70b`; analyzer: local
`../rust-quality-lens/target/debug/rqlens`, clean revision `2910143`.
Both measurements used the same analyzer and Rust 1.95 toolchain.

## Changes

- `src/audio/controller.rs` now owns the control thread, batching, reconnect
  behavior and publication, previously embedded in `src/api/effects.rs`.
  A batch has one execution/publication path; mute and direction boundaries
  still split batches. Failed effects are never replayed. The PipeWire connection
  is now private to audio. Architecture rules include the new audio submodule.
- API domain reads use `StateStore::read`, not a full `BarSnapshot` clone.
  Only `bar.snapshot` still requests the complete snapshot. Serialization-only
  projections borrow their domain; operations clone only the domain they need.
  Removed unused API `Clone` implementations and the unnecessary Hyprland `Arc`.
- `src/api/battery.rs` shares device selection, editable-policy validation,
  runtime rollback and response publication. Removed the one-consumer
  `DeviceSupportError` adapter from the battery domain. Error codes/messages,
  effects locking, persistence-before-hardware and rollback ordering are retained.
  Regression coverage checks default/explicit/missing/unsupported devices.
- Battery forecast/history DTOs now live with the other battery data in
  `src/model.rs`, not beside the calculations in `src/battery/derived.rs`.
  This removes the model-to-calculation dependency cycle without a forwarding facade.
- Display layout/docking refresh is a named transaction; the monitor retains
  cancellation, locking and publication. Failed layout recovery still attempts
  fallback restoration before surfacing the error.
- LED sampling and watcher construction are separated from the monitor loop.
  Reads remain on the blocking pool, with the same polling/discovery intervals.
  `None` notifications retain existing watchers; `Some(empty)` clears them.
- Removed four glob imports, including both production globs; retained the
  generated-Wayland lint allowances. No lint waivers were added.
- `cargo fmt` also normalized pre-existing formatting in `src/compositor.rs`,
  `src/daemon/tasks.rs` and `src/lib.rs`.

## Before / after

Project-wide function sums include tests and newly extracted helpers; module rows
are not counted again. These are static observations, not measured execution cost.

| Observation | Before | After |
| --- | ---: | ---: |
| Cognitive complexity sum | 1,763 | 1,734 |
| Cyclomatic complexity sum | 3,600 | 3,593 |
| Function hotspot-score sum | 15,331.71 | 15,173.43 |
| Duplicate nonblank lines | 1,353 | 1,290 |
| Duplication percentage | 5.99% | 5.71% |
| Escape-hatch occurrences | 29 | 25 |
| Physical Rust lines, `src/` + `tests/` | 24,330 | 24,323 |
| Reliability findings (including tests) | 453 | 453 |
| Model outgoing dependencies | 10 | 9 |
| Model locality score | 70.75 | 72.25 |

`display_policy::monitor`: cognitive 18 → 12, cyclomatic 18 → 10,
pressure 136.81 → 80.25. Its extracted refresh has cognitive 4 and cyclomatic 9;
this is not a claim that all eight decisions disappeared.
`osd_hardware::monitor`: cognitive 18 → 5, cyclomatic 9 → 4,
pressure 98.78 → 32.20.

### Leverage/locality trade-offs

Function reuse improves through shared battery validation/rollback and the
existing state projection API. RQLens's *module* observed-reuse score does not
measure that: common-module leverage sum remains 2,120. The new audio controller
has one consumer (score 10); project mean changes 24.65 → 24.48. Derived battery
leverage falls 20 → 10 because the unwanted model consumer was removed; sleep
rises 80 → 90 with explicit imports. This is **not an aggregate leverage win**.

Common-module locality sum is unchanged. Model locality improves, offset by
additional observed incoming references to `state` and `activity`; the new audio
controller scores 100. The project mean 97.58 → 97.60 is denominator-sensitive,
not proof of a broad architecture improvement. No composition-root baseline was
raised or dependency hidden behind a facade.

RQLens supplies no Halstead/developer-effort metric. Hotspot pressure is only a
maintenance-pressure proxy. Source-size savings are modest because regression
coverage was retained and extended.

## Verification and limitations

Passed:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --all-targets -- --include-ignored`: **150 passed**, none
  ignored, including private PipeWire and native Hypridle fixtures.
- `cargo test --locked --doc` (no doctests)
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps`
- `git diff --check`

Reran RQLens producers: `hotspots`, `clones`, `escape-hatches`, `reliability`,
`locality`, `leverage`, `architecture-rules`, with `--config rqlens.toml`.
Baseline artifacts are in `/tmp/bar-rql-before/`; current artifacts are in
`target/analysis/` (both local, not committed).

All **843/843** current dependency references resolve; the three architecture
rules report **zero violations** (baseline: 817/817, also zero). However,
**`rqlens check` fails** because architecture evidence is partial: this analyzer
propagates unexpanded `state_updates!`/Wayland macros and block-local impl inventory
limitations. The baseline has the same partial-evidence limitation. No exemptions
were added to make the gate green. Other unrerun artifacts in `target/analysis/`
are stale and must not be treated as fresh verification/coverage evidence.

Remaining review targets include `sleep_policy::lid::connected` (pressure 161.95),
`display_policy::reconcile` (121.24), and the large transport routing match.
Coverage, allocation benchmarks and the declared Rust 1.85 MSRV were not verified
in this pass. Production panic-path findings remain for a separate invariant review.
