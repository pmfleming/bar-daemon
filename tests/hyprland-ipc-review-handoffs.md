# Review issue 3: retain manual moves to every destination

Handoff intake records a numbered destination or an explicit numberless
alternative for each moved address. Special/named destinations never compare
equal to a numbered lease. The bounded set retains earlier departures even if
a window returns or disappears in the same reconciliation batch; overflow still
fences all observers. Ordinary valid moves do not become global event loss.

Validation: `cargo test --locked workspace:: -- --test-threads=1` passes. The
manual-move/return/replacement test covers numbered, special and named v1 moves;
intake separately verifies special identity is recorded without a loss flag.
All existing pinned identity, restart, close and replacement tests remain green.
