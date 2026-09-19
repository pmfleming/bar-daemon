# Update worker status

The privileged `update-daemon` project provides finite systemd-scheduled jobs.
bar-daemon never executes its helpers and exposes no privileged update command.
Existing `updates.changed` events and snapshot `updates.jobs` report read-only
job outcomes from `/var/lib/nixos-delayed-updates-v2/jobs/{system,ai-tools,ai-tools-stale}.json`.

The existing recursive filesystem watch covers atomic job writes, with a
60-second refresh fallback. A bounded schema-v1 reader rejects malformed,
oversized and symlinked records. Boot ID and Linux process start identity prevent
crashed/rebooted/PID-reused workers from appearing permanently active. Completed
status does not imply installation: phase records distinguish checking, blocked,
quarantined, ready, staged for boot, current and activated outcomes. A successful
AI update supersedes an older stale-check warning without waiting for its timer.
Only the current delayed lane contributes to `updates.ready`; obsolete fast-lane
artifacts are ignored.

Shelllist shows active work and failures/staleness even when no candidate is
ready. Clicking opens the existing NixOS/AI-tool service journals. The worker
retains the existing independently rate-limited AI desktop alerts. No signal to
an old bar process is needed for filesystem-driven status refresh.
