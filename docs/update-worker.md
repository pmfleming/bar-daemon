# Update worker status

The privileged `update-daemon` project provides finite systemd-scheduled jobs.
bar-daemon exposes no privileged update command. It reads worker ledgers and may
invoke only the installed worker's unprivileged `status --json` report.
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

Shelllist's Updates viewer shows active work and failures/staleness even when no
candidate is ready. A resident SNI item opens the read-only viewer; scoped logs
remain explicit reads. Desktop/legacy notification ownership is configured by
the worker, never inferred from session presence. See [Updates](../../shelllist/docs/updates.md).

`updates.getOverview` takes no parameters. It invokes the absolute installed
`/run/current-system/sw/bin/update-worker status --json` executable (override:
`BAR_DAEMON_UPDATE_WORKER`), with no shell, sudo or caller-selected arguments.
It normalizes booted/active/profile systems, validated boot staging and known age,
baseline validity, tool versions/successful checks and actual systemd next times.
Missing evidence remains null; historical staging is never used as current state.
The report is bounded to 256 KiB, two readers and a five-second child deadline
inside a six-second admission/read budget. The matching worker retires its report
and nested subprocess groups after four seconds. Errors remain separate from
readable history; older workers may lack added timestamps.

The same bounded catalog provides up to 50 latest unresolved issues, with total
and partial-read diagnostics. Recovery annotations are display-only: later
matching component success recovers earlier failures, without rewriting outcomes.
Holds, running/interrupted/cancelled runs and equal timestamps do not establish
recovery. Incident identities are validated; a different component's success
cannot erase an issue. Manual reports expose nullable `failed_stage` and
`deployed`; search includes outcome codes as well as recorded fields.
