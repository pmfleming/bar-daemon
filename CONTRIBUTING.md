# Contributing

## Development environment

Use the pinned Nix development shell so PipeWire headers, libclang, and Rust tooling are available:

```sh
nix develop
```

## Required checks

Before submitting a change, run:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- --deny warnings
cargo test --workspace --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Changes to `bar-api` must update `test_support/bar-api-v1.json` and preserve the protocol registry tests. Keep system effects behind domain modules and add tests for policy and normalization logic that does not require desktop hardware.

Use focused commits and document user-visible changes in `CHANGELOG.md`.
