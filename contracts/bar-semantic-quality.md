# Bar semantic facts — bounded quality review

Implementation remains **partial**: the acquisition-validity row in
[bar-semantic-facts.md](bar-semantic-facts.md) is not implemented by this review.
No integration, hardware, default-switch or rollout approval follows.

## Scope and method

Original baseline `83658f4d537850941765bae10c4fc8ed3ac27b6e` to clean implementation
`f5b5b97e0a5860e14dbfd2f4b14ea7f9fbbda15e` includes seven Rust files, the checked
JSON companion and the contract, not only the last documentation commit. No seeds.
Whole changed Rust files, including `power/automation_tests.rs` and
`protocol/control_fixtures.rs`, were measured. Other generated JSON inputs are
fingerprinted; unexpanded derives are not claimed as compiler-complete inventory.

Pinned RQLens `d28f3d936fa3c0d4c0a7ea753f436517077cb37d`, CLI SHA-256
`622b2c793a574781cd40a190a457f58af4b8c222ee5efce04c4e77488c50929a`.
Task-local static config uses the pinned tool's `rust_helpers/Cargo.toml`.
Normal analyzer-managed **locked** rust_facts/ast_hasher compilation occurred in
the task's private `quality/cache`, helper digest `6a098384d350404d`. No tool source,
main CLI, lock or project config changes. Runtime/audit/coverage/compiler-expansion
campaigns were not run; semantic identity is explicitly disabled/partial.

Artifacts/configs/triage/exact drivers and logs are under
`/home/laufan/Projects/shelllist-orchestration/20261026T202406Z/tasks/05-bar-semantic-facts/quality/`:
`before/rust`, `after/rust`, and clean-quality-head `final/rust`.

## Findings and disposition

- Six static producers: hotspots, clones, reliability, type-health, test-quality,
  map. Seven authored Rust files extracted; 91 function/file hotspot records,
  78 reliability advisories, 48 type-health records and 11 discovered test bodies.
- Four token-clone groups / 61 duplicated lines (2.43%) remain. One new group is
  private server/tempdir/envelope setup in the removed-target test. Retain explicit
  lifetimes and distinct tests rather than build abstractions for six setup lines.
  Other groups are existing restart setup, connection setup and fixture objects.
- Seven changed-hunk unwrap findings are test-only private fixture creation,
  connection, reconciliation or fake-state mutex checks. Retain fail-fast test
  behavior. The other 71 findings are at retained locations; this is hunk
  attribution, not proof that every retained finding is harmless.
- Native manual-profile support had three independent lists after implementation.
  Share one ordered `MANUAL_PROFILES` constant across eligibility, the public
  setter and owner revalidation. Both guards and the pre-persistence/pre-release
  ordering remain. This reduces contract drift without moving an effect, splitting
  the coordinator or changing supported profiles/automation policy.
- Metrics intentionally do not claim a numerical win: next_manual_profile stays
  cyclomatic 4 / cognitive 2 (12 → 11 source lines); set_profile stays 4/1 and
  select_manual stays 9/4. Clone/reliability counts remain unchanged.

Type-health remains partial for derives/cfg/unexpanded macros; test discovery adds
unchanged Wayland macros from integration tests. Narrow-root module attribution
and disabled semantic identities are not compiler ownership. Map has seven unknown
aggregate module metrics and missing correctness/locality/leverage inputs; no
architecture-rule or aggregate quality pass is claimed. Existing ignored/helper-
assertion test advisories are not removed or made passing by static evidence.

## Correctness after refactor

Through the orchestration shared lock and private environment, the matching
sibling-source driver `quality/check-refactor.sh` passed: 226 ordinary library,
29 update-worker, one ordinary native integration, strict Clippy/format/build,
all producer/frontend fixture and protocol parity, private native Quickshell
probe, TS compile/generated parity, QML lint and 460 actual Qt passes / one existing
skip. Matched Nix `barDaemonContract` passes 231 release library + 30 update-worker
+ two native integrations; `qmlTests` has 460 passes / one existing skip;
`buildInputs` passes. No effect test reaches physical hardware or host buses.

Quality evidence is **partial**, not missing: the BEFORE/AFTER loop ran and the
scoped refactor passed affected checks, while uncertainty above remains explicit.
The original UPower/sysfs percentage/health validity gap and unrelated retained
brightness-fixture ETXTBSY observation remain open. Private correctness timings
are not final latency/memory measurements. Coordinator review is still required.
