"""Evidence orchestration contracts; no analyzer or desktop services are launched."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

SCRIPT = Path(__file__).with_name("quality-evidence.sh").resolve()

class EvidenceRunnerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        (self.root / "tools").mkdir()
        shutil.copyfile(SCRIPT, self.root / "tools/quality-evidence.sh")
        self.config = 'project_root = "."\noutput_dir = "target/analysis"\n[verification]\ninclude_ignored = true\nlocked = true\n'
        (self.root / "rqlens.toml").write_text(self.config)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=Test",
                        "-c", "user.email=test@example.invalid", "commit", "-qm",
                        "fixture", "--allow-empty"], check=True)
        self.out = self.root / 'evidence "spaces"'
        self.env = dict(os.environ, RQLENS_BIN=str(self.root / "lens"),
                        QUALITY_EVIDENCE_DIR=str(self.out))
        self.program("tools/quality-fixtures.sh", '''
mkdir "$QUALITY_OUTPUT_DIR"
printf 'fixture evidence\n' > "$QUALITY_OUTPUT_DIR/complete.txt"
exit "${FIXTURE_STATUS:-0}"
''')
        self.program("lens", '''
printf '%s\n' "$*"
mkdir -p "$QUALITY_EVIDENCE_DIR/analysis"
printf '{}\n' > "$QUALITY_EVIDENCE_DIR/analysis/policy_report.json"
case "$1" in
  verify)
    if [[ "${REPLACE_LENS:-0}" == 1 ]]; then printf '#!/bin/sh\nexit 9\n' > "$RQLENS_BIN"; fi
    exit "${VERIFY_STATUS:-0}";;
  check) exit "${POLICY_STATUS:-0}";;
esac
''')

    def program(self, name, body):
        path = self.root / name
        path.write_text("#!/usr/bin/env bash\nset -eu\n" + body)
        path.chmod(0o755)

    def run_script(self):
        return subprocess.run(["bash", str(self.root / "tools/quality-evidence.sh")],
                              env=self.env, text=True, capture_output=True)

    def test_success_requires_all_phases_and_refuses_reuse(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.out / "complete.txt").is_file())
        self.assertTrue((self.out / "analysis/policy_report.json").is_file())
        self.assertEqual((self.out / "status.tsv").read_text().splitlines(),
                         [f"{phase}\t0" for phase in
                          ["fixtures", "verify", "measure", "policy", "report"]])
        policy = (self.out / "policy.log").read_text()
        for gate in ["practice-failure", "test-failure", "partial"]:
            self.assertIn(f"--fail-on {gate}", policy)
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertEqual((self.root / "rqlens.toml").read_text(), self.config)
        derived = tomllib.loads((self.out / "rqlens.toml").read_text())
        expected = dict(tomllib.loads(self.config), project_root=str(self.root),
                        output_dir=str(self.out / "analysis"))
        self.assertEqual(derived, expected)
        self.assertFalse((self.root / "target/analysis").exists())

    def test_running_binary_is_not_changed_by_a_tool_rebuild(self):
        self.env["REPLACE_LENS"] = "1"
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(subprocess.run([self.env["RQLENS_BIN"]]).returncode, 9)

    def test_any_failed_phase_preserves_evidence_without_certifying_success(self):
        for variable, phase, code in [
            ("FIXTURE_STATUS", "fixtures", 7),
            ("VERIFY_STATUS", "verify", 7),
            ("POLICY_STATUS", "policy", 1),
        ]:
            with self.subTest(phase=phase):
                self.env[variable] = str(code)
                self.env["QUALITY_EVIDENCE_DIR"] = str(self.root / variable)
                result = self.run_script()
                out = Path(self.env["QUALITY_EVIDENCE_DIR"])
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse((out / "complete.txt").exists())
                self.assertTrue((out / "analysis/policy_report.json").exists())
                self.assertEqual((out / "status.tsv").read_text().splitlines(),
                                 [f"{name}\t{code if name == phase else 0}" for name in
                                  ["fixtures", "verify", "measure", "policy", "report"]])
                del self.env[variable]

if __name__ == "__main__":
    unittest.main()
