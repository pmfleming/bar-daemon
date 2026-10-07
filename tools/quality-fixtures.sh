#!/usr/bin/env bash
# Run inside the repository's Nix development shell. Never use host services.
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-test}"
case "$mode" in
  test|coverage) ;;
  *) echo "usage: $0 [test|coverage]" >&2; exit 2 ;;
esac
command -v pipewire >/dev/null
command -v dbus-daemon >/dev/null
: "${HYPRIDLE_TEST_BIN:?enter the Nix dev shell to supply patched Hypridle}"
[[ -x "$HYPRIDLE_TEST_BIN" ]] || { echo "Hypridle is not executable: $HYPRIDLE_TEST_BIN" >&2; exit 1; }

# Refuse reuse: stale logs/coverage must never stand in for this execution.
if [[ -n "${QUALITY_OUTPUT_DIR:-}" ]]; then
  out="$QUALITY_OUTPUT_DIR"
  mkdir -p "$(dirname "$out")"
  mkdir "$out"
else
  mkdir -p target
  out=$(mktemp -d target/quality-fixtures.XXXXXX)
fi
args=(test --workspace --all-targets --locked)
if [[ "$mode" == coverage ]]; then
  args=(llvm-cov --workspace --all-targets --locked --cobertura --output-path "$out/cobertura.xml")
fi
args+=(-- --include-ignored)
{
  printf 'command: cargo'; printf ' %q' "${args[@]}"; printf '\n'
  printf 'revision: '; git rev-parse HEAD
  git status --short
  cargo --version
  rustc --version
  printf 'pipewire: %s\ndbus-daemon: %s\nHYPRIDLE_TEST_BIN: %s\n' \
    "$(command -v pipewire)" "$(command -v dbus-daemon)" "$HYPRIDLE_TEST_BIN"
} > "$out/context.log"
printf 'Fixture evidence: %s\n' "$out"
cargo "${args[@]}" 2>&1 | tee "$out/tests.log"

# A successful filtered/default Cargo run is not full-fixture evidence.
for test in \
  audio::tests::private_pipewire_control_reuses_connection_and_reads_external_changes \
  native_control_preserves_cookies_and_base_listeners_and_cancels_activity; do
  grep -Fx "test $test ... ok" "$out/tests.log" >/dev/null || {
    echo "Required fixture did not pass: $test" >&2; exit 1;
  }
done
if grep -Eq 'test result:.*; [1-9][0-9]* ignored;' "$out/tests.log"; then
  echo 'Full-fixture run unexpectedly ignored tests' >&2
  exit 1
fi
if [[ "$mode" == coverage ]]; then
  test -s "$out/cobertura.xml"
fi
printf 'Both required fixtures passed; no ignored tests.\n' > "$out/complete.txt"
