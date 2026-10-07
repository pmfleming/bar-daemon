#!/usr/bin/env bash
# Run inside the development shell. Keep failed/partial evidence; never certify it.
set -euo pipefail
cd "$(dirname "$0")/.."
lens=$(realpath "${RQLENS_BIN:-../rust-quality-lens/target/debug/rqlens}")
[[ -x "$lens" ]] || { echo "RQLens is not executable: $lens" >&2; exit 1; }
if [[ -n "${QUALITY_EVIDENCE_DIR:-}" ]]; then
  out="$QUALITY_EVIDENCE_DIR"
  mkdir -p "$(dirname "$out")"
  mkdir "$out" # Refuse reuse, including old logs or a stale complete marker.
else
  mkdir -p target
  out=$(mktemp -d target/quality-evidence.XXXXXX)
fi
# A parallel tool rebuild cannot change the executable halfway through the run.
cp "$lens" "$out/rqlens"
# Isolate this run from stale or optional experimental artifacts. Preserve every
# policy/selection setting; only resolve the project root and choose a fresh output.
python3 - "$out" <<'PY'
import json
from pathlib import Path
import re
import sys
import tomllib

out = Path(sys.argv[1]).resolve()
text = Path("rqlens.toml").read_text()
original = tomllib.loads(text)
changes = {"project_root": str(Path(original["project_root"]).resolve()),
           "output_dir": str(out / "analysis")}
for key, value in changes.items():
    text, count = re.subn(rf"^{key}\s*=.*$", lambda _: f"{key} = {json.dumps(value)}",
                          text, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"Expected exactly one explicit top-level {key}")
if tomllib.loads(text) != dict(original, **changes):
    raise SystemExit("Evidence config changed settings beyond root/output paths")
(out / "rqlens.toml").write_text(text)
PY
config="$out/rqlens.toml"
{
  printf 'revision: '; git rev-parse HEAD
  git status --short
  cargo --version
  rustc --version
  printf 'RQLENS_BIN: %s\n' "$lens"
  sha256sum rqlens.toml "$config" "$out/rqlens"
} > "$out/context.log"
printf 'Quality evidence: %s\n' "$out"
failed=0
run() {
  local phase="$1" code
  shift
  { printf '%s:' "$phase"; printf ' %q' "$@"; printf '\n'; } >> "$out/commands.log"
  if "$@" 2>&1 | tee "$out/$phase.log"; then
    code=0
  else
    code=$?
    failed=1
  fi
  printf '%s\t%d\n' "$phase" "$code" >> "$out/status.tsv"
}

# Continue collecting independent evidence after failures. The exit code and
# complete marker still require every phase, including the strict gate, to pass.
run fixtures env QUALITY_OUTPUT_DIR="$out/fixtures" bash tools/quality-fixtures.sh coverage
run verify "$out/rqlens" verify --config "$config"
run measure "$out/rqlens" measure all --config "$config"
run policy "$out/rqlens" check --config "$config" \
  --fail-on practice-failure --fail-on test-failure --fail-on partial
run report test -s "$out/analysis/policy_report.json"
if (( failed )); then
  echo "Quality evidence is incomplete or failed; see $out/status.tsv" >&2
  exit 1
fi
printf 'Full fixtures, fresh measurements and strict policy passed.\n' > "$out/complete.txt"
