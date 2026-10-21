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
