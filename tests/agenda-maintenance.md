# Offline Agenda export and recovery

These are supported **offline** logical transfers, not a live file-copy procedure.
Manually stop the daemon using your normal service/session management first, and
prevent automatic restart for the maintenance window. The tool never stops,
starts, or contacts services. An active Agenda owner makes export fail on the
same exclusive ownership lock; a lock file's mere existence does not mean busy.
Do not delete lock, DB, WAL, or SHM files to bypass a refusal.

## Export

Determine the configured dedicated Agenda store directory; there is deliberately
no implicit live path. Use absolute paths without `..`. The source directory and
backup's existing parent directory must be owned by you, private (0700), and not
symlinks. The output must be a new file **outside** the source store directory.

```sh
bar-daemon agenda export --store-dir /absolute/private/agenda \
  --output /absolute/private/backups/agenda-2026-10-27.json
```

Missing/non-regular/empty DBs, unsupported schemas, corruption, ownership-lock
contention, unsafe paths, and existing outputs are errors, not empty successful
backups. The store's existing-only open mode must not initialize a database.
The logical snapshot is read through the engine under exclusive ownership,
including committed WAL recovery; no database-file copy is performed. Export
opens the engine read-only and closes without checkpointing the source DB/WAL. The new
backup is mode 0600 and fsynced by the store. Any error means **do not treat the
output as a verified backup**; an interrupted or failed write can leave a partial
new output that will not be overwritten on retry. Choose a fresh output name.

The export includes persisted records/children, IDs, revisions, source metadata,
catalog revision, operation receipts/fingerprints, and migration provenance
(including the original legacy JSON bytes). It does not include read-only
provider files/configuration, service configuration, or credentials. Keep those
originals and the migration's original private backup separately. Imported
calendar records remain provider-owned; this is not ICS round-trip/export.

## Verify and restore

Use a **new, nonexistent** destination directory under an existing private
parent. Even an existing empty directory or empty initialized database is
refused. Never restore over the old store; retain it and the original export.

```sh
bar-daemon agenda restore --store-dir /absolute/private/agenda-recovered \
  --input /absolute/private/backups/agenda-2026-10-27.json
bar-daemon agenda export --store-dir /absolute/private/agenda-recovered \
  --output /absolute/private/backups/agenda-verified.json
```

Restore validates application/schema identity and records using the actual
engine, commits atomically into the fresh initialized database, then checkpoints
and releases ownership. Destination directory is 0700; database/lock/backup
files are 0600. Input must be a private owned regular file in a private directory,
not a symlink; it is read, never modified/moved/deleted. Compare the two exports
as JSON values (object-key order is not meaningful). Retain the original backup
and source until recovery is verified. A failed restore leaves its new directory
for diagnosis; use another new directory for retry, not `--force` (there is none).

After successful verification, select the recovered store through the normal
configuration/deployment procedure, retaining original provider configuration;
then restart the daemon manually. Moving recovered directories or changing
configuration is an operator action, not part of this CLI. A backup is point-in-
time: it cannot include edits committed after export. **Restoring stale legacy
JSON alone is not lossless rollback of post-migration edits.** Downgrading to a
binary that cannot read the schema is not a supported rollback procedure.

## Isolated executable checks

Run through the orchestration `check.sh` private HOME/XDG/disconnected-session
wrapper and the captured daemon development environment; never point tests at
real user stores. Focused commands (Cargo jobs bounded to two by wrapper):

```sh
cargo test --lib agenda_maintenance::tests -- --test-threads=2
cargo test --bin bar-daemon tests::agenda_ -- --test-threads=2
```

Module tests perform actual Store migration/create/patch/delete/export/restore,
compare full logical export values, verify revision/receipt/source fidelity and
original bytes, recover a committed WAL after an actual abrupt subprocess exit,
and check lock/corruption/new-schema/missing-source/no-overwrite/permissions/
failed-restore refusal paths. Binary tests parse the real clap routes and dispatch
them through the same entry function into temp-dir Store operations; missing or
wrong flags and `--force` are rejected. External binary smoke checks additionally
exercise process exit status and output files. No test starts/stops services.

Limits: these checks do not qualify filesystem power-loss durability, network
filesystems, malicious same-user concurrent filesystem changes, live service
management, or recovery of unknown future schemas. Store limits are 10,000
records, 100,000 retained receipts, and 100 MiB combined stored payload; restore
inputs are capped at 512 MiB. Capacity errors never silently discard history.
This release does not prune receipts or provide legacy-JSON reverse conversion. Keep private verified backups
and original configuration; do not manually copy only `agenda.db` while WAL may
contain committed data.
