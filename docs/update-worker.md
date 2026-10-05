# Update worker status

The privileged `update-daemon` project provides finite systemd-scheduled jobs.
bar-daemon never executes its helpers and exposes no privileged update command.
Existing `updates.changed` events and snapshot `updates.jobs` report read-only
job outcomes from `/var/lib/nixos-delayed-updates-v2/jobs/{system,ai-tools,ai-tools-stale}.json`.

The recursive filesystem watch covers atomic job writes without reacting to its
own file reads. Invalidations coalesce over a fixed 75 ms window, retaining a
follow-up refresh for changes during a read. Directory device/inode identity is
checked before each refresh so replacement paths are rewatched; missing state
directories are watched through their nearest existing parent. Watch errors and
overflow trigger reconstruction, with two-second retries when no watch can be
installed and a 60-second reconciliation fallback when watching successfully.
A bounded schema-v1 reader rejects malformed,
oversized and symlinked records. Boot ID and Linux process start identity prevent
crashed/rebooted/PID-reused workers from appearing permanently active. Completed
status does not imply installation: phase records distinguish checking, blocked,
quarantined, ready, staged for boot, current and activated outcomes. A successful
AI update supersedes an older stale-check warning without waiting for its timer.
Only the current delayed lane contributes to `updates.ready`; obsolete fast-lane
artifacts are ignored. Readiness matches the worker's completeness check:
`ready-flake.lock`, `ready-revision`, `ready-base-hash`, `ready-created-at`, and
the `system` symlink must all be present.

Shelllist shows active work and failures/staleness even when no candidate is
ready. Clicking opens the existing NixOS/AI-tool service journals. The worker
retains the existing independently rate-limited AI desktop alerts. No signal to
an old bar process is needed for filesystem-driven status refresh.
