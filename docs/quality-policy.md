# Quality policy

Quality measurements guide design review; they are not targets to game by adding forwarding modules.
The verified gates in `rqlens.toml` and CI remain authoritative.

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

Architecture CI enforcement is still pending: the current checker reports 14
unresolved macro-generated Wayland references in `sleep/wayland_lock.rs`,
`sleep/wayland_lock_tests.rs`, and `tests/native_idle.rs`. Measurement reports no violations but partial
evidence, so `rqlens check` correctly fails. Resolve that analysis limitation
before adding a hard CI gate; do not disable identity resolution or count a
partial measurement as a pass. Existing fan-out baselines have not been raised.
See [the sleep follow-up review](sleep-quality-review.md) for the latest measured
changes, including aggregate complexity/leverage trade-offs.

The `webpki-roots` CA-root data uses CDLA-Permissive-2.0, accepted in `deny.toml`.
Its license text is retained in `packaging/licenses/` and installed with Nix binary
packages under `share/licenses/bar-daemon/`, as required when redistributing the data.

Lower baselines when fan-out is removed. Never raise them without recording the reason in the
change that introduces the dependency. Coverage, reliability, architecture-rule, dependency,
and test failures are not waivable through this composition-root baseline.
