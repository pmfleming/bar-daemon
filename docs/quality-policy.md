# Quality policy

Quality measurements guide design review; they are not targets to game by adding forwarding modules.
The verified gates in `rqlens.toml` and CI remain authoritative.
See the [media ownership and fixture reuse review](quality-review-3f18c38.md) for the latest
measurements, trade-offs and the current analyzer's partial-evidence gate failure.
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
rather than skip. RQLens's default verification test check is not a substitute
for this explicit full-fixture run.

Each invocation writes a new `target/quality-fixtures.*` directory with the exact
command, revision/dirty status, tool versions, prerequisite paths, and test log.
Coverage additionally writes `cobertura.xml`; `complete.txt` appears only after
validation. `QUALITY_OUTPUT_DIR` can name a new directory, but reuse is rejected.
Use identical selection on both sides of a comparison. The saved
`target/refactor-review/tests-before.log` ignored two fixtures; `tests-after.log`
ran both successfully (191 unit and two integration tests, none ignored). Do not
count the extra executions as newly authored tests or infer a baseline pass.

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
