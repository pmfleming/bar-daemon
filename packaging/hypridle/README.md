# Managed Hypridle

This is the single package definition and patch set used by both bar-daemon's
native-idle regression and Shelllist's Home Manager runtime service. It moved
from Shelllist's `nix/hypridle-ready.nix`; do not retain a second patch copy there.

- `lib.mkManagedHypridle package` preserves an explicitly configured upstream
  package while adding readiness and live timeout-control support.
- `packages.<system>.managedHypridle` applies it to the pinned nixpkgs Hypridle.
- The development shell and Rust package checks set `HYPRIDLE_TEST_BIN` to that
  package's executable. CI coverage includes ignored fixtures; it must not skip
  native idle or silently substitute an unpatched binary.

The readiness patch also tracks the selected `--config` file when checking source
cycles. Hypridle 0.1.7 otherwise looks up a default configuration even with an
explicit config, failing on clean machines. The integration test starts with an
empty isolated home/config directory and validates the native readiness message.

Run from the development shell:

```sh
cargo test --locked --test native_idle -- --ignored
```

The test owns its D-Bus policy (`test_support/dbus-session.conf`), mock login
service, Wayland display, and notification socket. No host bus, compositor,
configuration, idle listener, or sleep action is involved. Child logs are reported
if readiness fails. Full sandboxed Nix package checks exercise this same fixture.
