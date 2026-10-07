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

    def test_ambiguous_config_is_rejected_before_execution(self):
        (self.root / "rqlens.toml").write_text(self.config + '[extra]\noutput_dir = "other"\n')
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse((self.out / "status.tsv").exists())
        self.assertFalse((self.out / "complete.txt").exists())

    def test_partial_policy_is_not_certified_but_evidence_is_preserved(self):
        self.env["POLICY_STATUS"] = "1"
        self.assertEqual(self.run_script().returncode, 1)
        self.assertFalse((self.out / "complete.txt").exists())
        self.assertTrue((self.out / "analysis/policy_report.json").exists())
        self.assertIn("policy\t1", (self.out / "status.tsv").read_text())

    def test_prior_failure_survives_passing_policy_and_tee(self):
        for variable in ["FIXTURE_STATUS", "VERIFY_STATUS"]:
            with self.subTest(variable=variable):
                self.env[variable] = "7"
                self.env["QUALITY_EVIDENCE_DIR"] = str(self.root / variable)
                result = self.run_script()
                out = Path(self.env["QUALITY_EVIDENCE_DIR"])
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse((out / "complete.txt").exists())
                statuses = (out / "status.tsv").read_text()
                self.assertIn("\t7\n", statuses)
                self.assertIn("policy\t0", statuses)
                self.assertIn("report\t0", statuses)
                del self.env[variable]


if __name__ == "__main__":
    unittest.main()
