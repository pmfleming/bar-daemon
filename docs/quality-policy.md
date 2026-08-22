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
```

Lower baselines when fan-out is removed. Never raise them without recording the reason in the
change that introduces the dependency. Coverage, reliability, architecture-rule, dependency,
and test failures are not waivable through this composition-root baseline.
