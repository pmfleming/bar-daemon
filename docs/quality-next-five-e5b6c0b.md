# Five quality follow-ups to e5b6c0b

Each change is independently tested and committed. RQLens is the local
`../rust-quality-lens/target/debug/rqlens`, copied once into
`target/quality-next-e5b6c0b/rqlens`. Configurations `step-0.toml` through
`step-5.toml` preserve repository settings, changing only project/output paths.
Step 0 is the fresh baseline. Generated evidence stays under `target/`.

## 1. Descriptor-validated history ingestion

Worker history previously checked ownership/mode using pathname metadata, then
opened the file again. `updates/files.rs` now checks the **opened inode**, sharing
nonblocking/no-final-symlink, regular-file, owner/mode, and read-limit enforcement
with manual history/log reads. This does not claim protection against replacement
of parent directories. History loading separates one-record validation/projection
from bounded traversal, samples retention time once, and uses catalog-owned
retention/error policies.

New tests cover wrong ownership, writable files, symlinks, FIFOs, directories,
size limits, retention boundaries, and valid-record survival after more than eight
bad records. Existing producer, manual-ledger, and notification tests still pass.

RQLens worker `history::load`: cognitive **23 → 8**, cyclomatic **22 → 13**,
Halstead effort **109,070 → 40,354**. Work moved into shared file operations and
`read_entry`; these are not eliminated decisions. Validation: **33 update tests**,
strict workspace/all-target Clippy, formatting, and diff whitespace checks passed.

## 2. Decode manual reports once

Manual ledger and marker readers now decode bounded reports through `Report`'s
source-specific constructors, then pass the parsed report to shared ingestion.
Markers no longer deserialize twice. Terminal-only ledger and running-only marker
rules remain explicit. Both readers reuse catalog retention and the eight-error
cap; the line/marker/directory limits and terminal-before-marker precedence remain.

Tests cover running ledger rejection, per-record size limits, invalid markers,
error saturation with valid records, and terminal precedence over a *live* marker
(as well as the existing abandoned-marker case).

RQLens manual `load`, compared with step 1: cognitive **19 → 14**, cyclomatic
**22 → 17**, effort **147,424 → 91,812**. Validation: **34 update tests**, strict
Clippy, formatting, and diff checks passed.
