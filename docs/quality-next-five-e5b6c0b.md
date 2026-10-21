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

## 3. Stream JSON measurement instead of retaining scratch buffers

A shared `updates/json.rs` writer hashes and counts serialized bytes without keeping
an extra JSON `Vec`. It replaces whole-catalog buffers in pagination and monitor
invalidation and per-entry buffers for page budgeting. Catalog revision inputs remain
entries/errors/availability. Serialization failures are propagated/logged instead of
being converted into the hash of an empty buffer.

The hash now consumes JSON bytes directly rather than `Vec::hash`'s length-prefixed
encoding. This intentionally invalidates old opaque cursors once; the existing
stale-cursor refresh contract applies. It is not a cryptographic digest. This is an
allocation improvement by construction, **not** a benchmarked latency claim. Size-only
measurements also compute the hash. RQLens page complexity stays **17 / 16**, while
its effort rises **254,978 → 262,309**; monitor cognitive complexity rises **21 → 22**
with explicit error handling (addressed structurally in step 5).

Tests compare streamed counts/hashes against serialized Unicode/escaped bytes,
reject serialization failure, exercise revision inputs, and ensure byte-limited
pages progress even with one oversized entry. **38 update tests**, strict Clippy,
formatting, and diff checks passed.

## 4. Shared, cancellation-safe blocking-read admission

`updates/reader.rs` provides two shared blocking-read slots for history pagination,
overview catalog scans, log lookup/manual tails, and background catalog scans.
Previously the endpoints used separate budgets and log lookup had neither admission
nor a timeout. Permits live inside the blocking closure: cancelling or timing out
its waiter cannot admit replacement filesystem work until that closure exits.
Waiting requests never spawn work before acquiring a slot.

Log lookup now has a three-second timeout, including admission; its existing
three-second tail/journal deadline remains separate. Overview still independently
limits external worker processes to two, without cloning an `Arc` permit. This is
not a global limit on all filesystem work (e.g. readiness reads), and does not make
blocking OS operations cancellable. Slow scans can now backpressure other readers;
that is the intended resource bound.

Deterministic channel/poll tests cover cancelled active readers, cancelled queued
waiters, and permit release on errors/panics. **40 update tests**, strict Clippy,
formatting, and diff checks passed. Query cyclomatic complexity **5 → 3**, effort
**12,812 → 7,150**; overview effort **49,128 → 45,951**. Log-read effort increases
**112,602 → 119,054** with its additional timeout boundary.
