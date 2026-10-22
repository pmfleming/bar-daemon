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
The report is bounded to 1 MiB (including bounded per-package release evidence),
two readers and a five-second child deadline
inside a six-second admission/read budget. The matching worker retires its report
and nested subprocess groups after four seconds. Errors remain separate from
readable history; older workers may lack added timestamps.

Optional `git` evidence describes only the worker-configured local repository:
branch/revision, cached upstream, ahead/behind, dirty/shallow state and local
observation time. It never represents a fresh remote check. The worker bounds
this read to 750 ms, suppresses optional index writes and lazy fetching, and
fences concurrent observations. Shallow graphs/no upstream leave distance null;
older workers or inaccessible repositories leave the capability unavailable.

Optional tool `freshness` retains latest-known version/release time, source,
comparison, stale/hold state, hold time/reason and installed origin. The worker
recomputes comparison from receipts, release evidence and packaging fingerprints;
serialized comparison fields are not trusted. Current/newer/ahead/rebuild/unknown
remain distinct. Job success is not freshness, and stale discovery is labeled.
No application release inventory or exact system release distance is fabricated.

Optional `applications` evidence carries active-generation package metadata:
`available`, `active_system`, `observed_at`, `packages` (name/scope/nullable version
and optional cached `release`),
`total` and `error`. The worker reads a generation-owned manifest, never evaluates
Nix or checks a release service. The projection validates up to 512 entries and
preserves total coverage (up to 4096 source entries). Missing old-worker support
stays null; a missing manifest is explicitly unavailable. These are system and
Home Manager declarations, not proof of successful home activation or the current
versions of independent/vendor profiles. Invalid payloads cannot become a
successful empty inventory. `release` preserves upstream version/source URL,
successful-check and attempted-check timestamps, stale state and error. Missing
old-worker fields stay null; failed checks may retain a last-known version but
cannot claim freshness. Invalid release metadata rejects the overview without
replacing the frontend's previous evidence. The worker's separate scheduled
`check-applications` command owns network acquisition and cache writes;
`updates.getOverview` still invokes only read-only `status --json`. No new D-Bus
mutation or network-on-refresh route is introduced.

The same bounded catalog provides up to 50 latest unresolved issues, with total
and partial-read diagnostics. Recovery annotations are display-only: later
matching component success recovers earlier failures, without rewriting outcomes.
Holds, running/interrupted/cancelled runs and equal timestamps do not establish
recovery. Incident identities are validated; a different component's success
cannot erase an issue. Manual reports expose nullable `failed_stage` and
`deployed`; search includes outcome codes as well as recorded fields.

`updates.queryHistory` additionally accepts nullable `lane` and `outcome` filters
and up to two validated `anchor_ids`. `problems` combines failed, partial, blocked
and interrupted outcomes. Filtering precedes pagination/counts but follows recovery
annotation; exact `run_id` bypasses filters/search. `retain_through` is the matching
prefix length needed to include surviving anchors (zero if absent), not another
unbounded page-size parameter. The caller keeps the same anchors throughout a
cursor chain. Cursors fence catalog contents, filters, anchors and resolved dates;
stale ones return `update-history-cursor-stale` without altering history.

Date terms use run start dates in the daemon's local timezone: `today`, `yesterday`,
`YYYY-MM-DD`, `on:DATE`, inclusive `since:DATE`, and exclusive `before:DATE`.
Named bounds also accept today/yesterday. Other words remain case-insensitive
AND substring terms. Impossible dates reject the read rather than silently
broadening it. Local calendar comparison handles DST and relative terms invalidate
cursors at midnight. Existing 100-record/128-KiB page and three-second read limits
remain; the UI stages bounded continuations and allows one stale-cursor restart.
