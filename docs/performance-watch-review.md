# Updates watcher performance review

## Evidence and change

Shelllist's external Rust lens, run `1792621617180-449244-0`, observed the
already-running bar-daemon at **96.83% of one CPU core** over a 20-second passive
capture. The hidden Quickshell host recorded no CPU ticks in that interval. The
busy daemon thread was named `notify-rs inoti`; repeated observations showed
replacement watcher threads. Ptrace was unavailable, so this is not a sampled
stack profile.

The manual ledger at `~/.local/state/nixos-rebuild` is flat, but the old watcher
registered it recursively with notify's default symlink following. Its
`check-cache/*/checks` links point into Nix store build/check closures. None of
those descendants are inputs to the manual ledger reader. Traversing them wastes
registration work and can repeatedly trigger watcher recovery on traversal
errors. Event filtering after registration does not avoid this cost.

The manual watcher is now nonrecursive. Worker-state watching still covers real
history/job subdirectories, but does not follow directory symlinks. Existing
read-event suppression, coalescing, reconciliation, missing-directory recovery,
publication and domain acknowledgement rules remain unchanged.

## Reproduction and validation

`updates::watch::tests` checks the actual Linux inotify registrations in
`/proc/self/fdinfo`: the flat ledger is watched, cache descendants and external
symlink targets are not, and the worker's real history subdirectory is watched.
The flat-ledger regression failed before the fix.

The ignored `profile_manual_watch_registration` test constructs a deterministic
1,000-directory synthetic closure outside a ledger, symlinks it from check-cache,
and times 25 real watcher registrations. Fixture setup is outside the timer.
On this host, in the same debug test profile:

- Before: **371.785 ms**.
- After: **3.318 ms** (about 112× less registration time in this fixture).

Run from the bar-daemon development environment:

```sh
cargo test --locked updates::
cargo test --locked updates::watch::tests::profile_manual_watch_registration -- --ignored --nocapture
```

These are isolated registration measurements, not a promise of a 112× faster UI.
The full Updates tests retain create/replace/remove recovery and terminal-ledger
publication. The live daemon was **not restarted or redeployed**; its measured
CPU reduction remains a deployment acceptance check. No update, notification,
power or display operation was issued by the review.

## Deployment verification (2026-10-22)

The fix was already deployed when this follow-up started; no service restart or
system switch was necessary. The running executable was
`/nix/store/813kqrax0vl0qjvzchf1cr6rg6x0jyq1-bar-daemon-0.1.0/bin/.bar-daemon-wrapped`
(SHA-256 `49fe1c22afe4c2b0583e7915306a55f6048edaadfcb79fd1f42db92c542cf1a8`).
Its derivation's source `if5hbyqlsfvkwqzs6wsxxrgpwnaj5q8c-source` contains a
byte-identical `src/updates/watch.rs` to commit `0b91219`.

Three independent 20-second passive release-lens captures (500 ms task cadence,
5 s detail, 1 s settling, no concurrent build) measured **1.590%, 1.538%, 1.692%**
of one core for the daemon. The two `notify-rs inoti` tasks retained the same IDs
across all three captures and recorded zero observed CPU ticks. Actual fdinfo
showed one registration on the manual ledger inode, plus five on the other
inotify descriptor; no enormous recursive closure registration remained.
Local artifacts: `/tmp/shelllist-perf-rollout/bar-captures/`, run IDs
`1792654214660-17479-0`, `1792654235706-17494-0`, `1792654256782-17499-0`.
The primary label is `ui-host` because the daemon was the explicit capture PID;
it is not a Quickshell measurement.

Release Updates tests: **45 passed, two opt-in probes excluded**. Three release
registration probes measured **2.749, 2.518, 2.673 ms** for 25 registrations.
These validate the deployed condition, not a matched causal A/B estimate against
the old 96.83% observation: the old build was not reintroduced into the desktop,
and the earlier registration timings used debug builds. Read-event suppression,
create/replace/remove recovery and publication remain covered by the tests.

## Bounded recovery follow-up

Watcher rebuilds now use framework sustained-health backoff (1–30 seconds,
reset only after 60 seconds healthy). Repeated errors/rescans cannot reset or
postpone an already scheduled retry. While watching is unavailable, read-only
reconciliation still runs at most two seconds apart, independently of backoff.

Recursive worker watches are planned as at most 512 explicit nonrecursive
registrations, without descendant symlink traversal. Exceeding the cap rejects
the entire watch plan and leaves polling active. Directory creation/removal and
watched-directory renames invalidate the registration plan; ordinary atomic file
replacement does not rebuild it. Logs expose registration count and degraded
recovery. The manual ledger remains flat. Release Updates tests: 47 passed, two
opt-in probes excluded, including real fdinfo cap checks, rebuild storms, new
subdirectory registration, and existing replacement/removal/publication tests.
These hardening changes are not yet deployed.

## Follow-up: default history projection

History pagination built and lowercased a complete search string (including JSON
serialization of every component) even for an empty query or date-only filtering.
`Search::matches_with` now evaluates date predicates first and invokes a lazy text
producer only when actual text terms remain. Serialization for revision fencing,
page byte limits and response payloads is unchanged. Exact-run targeting, native
ordering, date/DST semantics, retention anchors and stale-cursor checks remain.

The ignored `updates::history::tests::profile_default_history_pages` measures eight
default pages over independently prepared 1,000-row catalogs with eight components
per row. In the same debug profile, timed projection decreased from **1719.152 ms
to 954.398 ms** (44% in this synthetic run). Fixture construction is excluded; this
includes fingerprinting, pagination, response construction and dropping consumed
catalogs, not filesystem/D-Bus/UI latency. The new lazy-producer test proves empty,
date-only and date-rejected queries never build text; combined text/date queries
build it once. All 45 Updates tests pass (two opt-in performance probes excluded).
