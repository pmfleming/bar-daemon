# Six-stage quality campaign (2026-10-30)

Start: clean `bd369b4873026abe84d9d8157655d2bdc2db064d` on `main`.
Only this repository is owned. No dependency pins, public protocol/fixtures,
services or live settings are changed. Generated evidence is ignored beneath
`target/quality-campaign-2026-10-30/` (abbreviated `E` below).

## Evaluator and environment

- Existing campaign RQLens 0.1.0, copied once to `E/rqlens` and reused throughout.
  SHA-256: `622b2c793a574781cd40a190a457f58af4b8c222ee5efce04c4e77488c50929a`.
  Its embedded tooling checkout is at `d28f3d936fa3c0d4c0a7ea753f436517077cb37d`;
  the binary hash is the evaluator identity, not the mutable sibling lens checkout.
- Unchanged `rqlens.toml` SHA-256:
  `9b6878ef47dcde5bef58ca4b3ae8edb8f5a5194719aca71390b0669b048b9f2a`.
  Derived configs change only absolute project/output paths. Measured package
  remains bar-daemon; mutable path dependencies also participate in fingerprints.
- Rust/Cargo 1.95.0, LLVM 21.1.8, Python 3.13.13; `CARGO_BUILD_JOBS=2`.
  Initial commands use `local-build develop . --command ...`. A later invocation
  refused a sibling worker's untracked file. Subsequent checks source the existing
  `.direnv/flake-profile-*.rc`: identical Rust, Python, PipeWire and patched
  `HYPRIDLE_TEST_BIN`, recorded in `E/cached-shell-identity.log`. No flake pins change.

## Original baseline (before any edit)

Commands: `bash tools/quality-evidence.sh`, with `RQLENS_BIN=$PWD/E/rqlens`
(expand `E`) and `QUALITY_EVIDENCE_DIR=E/baseline`; separately
`cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`,
and `python3 -B -m unittest discover -s tools -p 'test_quality_*.py'`.

- All **333 tests pass**, zero failed/ignored: 299 library, two CLI, 30 location,
  and two integration cases. This includes the `bd369b4` inherited-descriptor
  ownership regression, private PipeWire, native idle and large-window fixtures.
- Formatting, all-target compilation and four Python runner tests pass.
- **Coverage collection fails**, despite the passing tests: the file-exhaustion
  child leaves its 65,536-byte hard file-size limit in place during LLVM exit.
  `E/baseline-fault-repro.log` independently reproduces a passing isolated fault
  test followed by a failed profile merge: parent profile 609,088 bytes, child
  truncated at exactly 65,536 bytes. Original baseline coverage is unavailable.
- Fresh hotspot syntax evidence is available in `E/baseline/analysis/hotspots.json`.
  Verification and three clone attempts refuse publication because mutable path
  dependency inputs change during measurement. The remaining full measurement
  was terminated after those baseline failures; it is not passing certification.
  `E/baseline/status.tsv` and phase/retry logs retain this distinction.
- Original tracked Rust: **54,818 physical / 52,708 nonblank lines**.
  File-role partition: production-bearing 42,909 / 41,077; test-only 11,909 / 11,631.
  Production-bearing files include inline tests and executable contract fixtures;
  this is an explicit file-role count, not a complete semantic production/test split.

## Initial refactor: scope the file-exhaustion resource limit

Keep the previous hard limit, lower only the soft limit during the real database
write, and restore it before abrupt exit. No Store destructor, shutdown or
checkpoint is introduced; the original failed-write, absent-receipt, empty-store
and successful-recovery assertions remain. No production code changes.

Pass: `cargo llvm-cov --lib --locked --cobertura --output-path
E/initial-fault-coverage.xml -- real_file_write_exhaustion_no_success_or_partial_receipt
--test-threads=1` (one selected test; profile merge and Cobertura export succeed).
This is a focused regression result, not full-suite coverage. Formatting passes.
The initial unsupported positional filter invocation is retained separately in
`E/initial-filter-error.log`; it is not counted as passing.
