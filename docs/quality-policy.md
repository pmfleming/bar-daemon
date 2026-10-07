# Quality policy

Quality measurements guide design review; they are not targets to game by adding forwarding modules.
The verified gates in `rqlens.toml` and CI remain authoritative.
See the [five-stage follow-up](quality-stages.md) for the latest changes and evidence.
The [media ownership and fixture reuse review](quality-review-3f18c38.md) records the
previous measurements, trade-offs and analyzer partial-evidence gate failure.
The [borrowing and locality review](quality-review-ff1b2c1.md) records the previous pass.
Earlier evidence is recorded in the [ownership and duplication review](quality-review-1e4e70b.md),
the [committed-RQLens quality pass](quality-pass-64a587d.md)
and the [domain simplification review](quality-review.md).

## Full-fixture evidence

Use the same runner locally and in CI, from this checkout:

```sh
../daemon-framework/tools/local-build develop . --command bash tools/quality-fixtures.sh test
../daemon-framework/tools/local-build develop . --command bash tools/quality-fixtures.sh coverage
```

The Nix shell supplies PipeWire, D-Bus and `HYPRIDLE_TEST_BIN` (patched Hypridle).
These tests use private services, not the running desktop. Both are marked
`#[ignore]`, so ordinary `cargo test` omits them even with dependencies installed.
The runner selects `--include-ignored`, checks both fixture names actually pass,
and rejects nonzero ignored counts. Missing prerequisites and test failures fail
rather than skip. `verification.include_ignored = true` now gives RQLens the same
selection for tests, correctness runs and coverage; this requires the local RQLens
follow-up described in [quality-stages.md](quality-stages.md). The dedicated fixture
runner still verifies prerequisite availability, fixture names and zero ignored tests.

Each invocation writes a new `target/quality-fixtures.*` directory with the exact
command, revision/dirty status, tool versions, prerequisite paths, and test log.
Coverage additionally writes `cobertura.xml`; `complete.txt` appears only after
validation. `QUALITY_OUTPUT_DIR` can name a new directory, but reuse is rejected.
Use identical selection on both sides of a comparison. The saved
`target/refactor-review/tests-before.log` ignored two fixtures; `tests-after.log`
ran both successfully (191 unit and two integration tests, none ignored). Do not
count the extra executions as newly authored tests or infer a baseline pass.

## Fresh evidence and strict certification

```sh
cargo build --locked --manifest-path ../rust-quality-lens/Cargo.toml --bin rqlens
../daemon-framework/tools/local-build develop . --command bash tools/quality-evidence.sh
```

This is also the CI runner. It collects full-fixture coverage, `rqlens verify` and all
standard measurements before checking practice failures, test failures, partial/stale
evidence and the configured architecture rules. Each run has a derived `rqlens.toml`:
only the project-root/output paths change, with parsed equality checking every other
setting. All producers write directly to its new `analysis/` directory, so stale or
optional experimental reports in `target/analysis` cannot contaminate certification.
Use that run's config when inspecting/rechecking its evidence.

RQLens fingerprints the inputs. The script copies the analyzer executable to avoid a
mid-run rebuild, records each phase's exit status, and retains reports even if the
gate fails. `QUALITY_EVIDENCE_DIR` must be new; `RQLENS_BIN` can select an analyzer.
Only a passing run writes the outer `complete.txt`. An inner fixture completion marker
certifies fixture execution, **not** complete architecture evidence or policy success.

Generated declaration counts/expansion observations are not complete body ownership or
architecture evidence. Unindexed generated methods remain partial and block certification;
keep that failure visible rather than suppressing macros or weakening thresholds.
Runner sources participate in RQLens fingerprints. Generated evidence stays in `target/`.

## Composition-root baseline

The following modules intentionally have high outbound fan-out because they assemble otherwise
independent capabilities:

| Module | Responsibility | Outbound baseline | Minimum locality |
| --- | --- | ---: | ---: |
| `api` | Route protocol methods to capability handlers | 12 | 79 |
| `daemon` | Build the D-Bus service and process lifetime | 8 | 91 |
| `daemon::tasks` | Own and cancel domain monitor tasks | 9 | 88 |

An increase above these outbound baselines requires architecture review. Do not move dependencies
behind a pass-through facade solely to lower a score. A valid change should instead keep domain
modules independent, preserve the rules in `rqlens.toml`, and explain why the composition root needs
another capability.

## Ratchet workflow

After an architecture change:

```sh
rqlens measure leverage --config rqlens.toml
rqlens measure locality --config rqlens.toml
rqlens measure architecture-rules --config rqlens.toml
rqlens check --config rqlens.toml
```

The domain-to-transport rule includes sleep, idle/lid policy, display
policy/layouts, work-area observation, and nested power/update modules. `StateStore`
may reference state types owned by policy/work-area modules; it must not invoke
system effects.

The earlier architecture CI baseline ran `measure architecture-rules` and `check`
with RustQualityLens's generated-symbol fix. At that revision, all **774 references**
resolve locally; the three configured rules have **zero violations**, complete
evidence, and a passing policy check. The previous 14 unresolved Wayland references
were analyzer limitations, not exemptions. `rust.identity_build_macros = true`
explicitly trusts this checkout and dependencies to execute build-time code;
protocol XML and test fixtures participate in content fingerprints. Keep this
opt-in restricted to trusted projects. Existing fan-out baselines have not been
raised; incomplete/stale architecture evidence still fails the configured gate.
CI must use a RustQualityLens revision containing this option and resolver fix.
See [the sleep follow-up review](sleep-quality-review.md) for the latest measured
changes, including aggregate complexity/leverage trade-offs.

The `webpki-roots` CA-root data uses CDLA-Permissive-2.0, accepted in `deny.toml`.
Its license text is retained in `packaging/licenses/` and installed with Nix binary
packages under `share/licenses/bar-daemon/`, as required when redistributing the data.

Lower baselines when fan-out is removed. Never raise them without recording the reason in the
change that introduces the dependency. Coverage, reliability, architecture-rule, dependency,
and test failures are not waivable through this composition-root baseline.
