# Review issue 1: pin recovery must not gate journal reads

Pin-prefix release now runs under the lifecycle mutex immediately before the
first handoff pin or close snapshot, not while loading the journal. A successful
release is remembered for this loaded owner. A failed/unknown release admits no
pins and can safely be retried; restart resets that state. Existing journal
fencing, close admission and handoff guards are unchanged.

Validation: `nix develop --no-write-lock-file -c cargo test --locked
workspace::tests -- --test-threads=1` passes all 23 lifecycle tests. Regressions
cover context/operation/projection reads without capability requests, failed
cleanup preventing pin admission, retry ordering, one cleanup per owner and
reset on restart. `cargo fmt` and `git diff --check` pass.
