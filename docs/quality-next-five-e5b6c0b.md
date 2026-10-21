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
