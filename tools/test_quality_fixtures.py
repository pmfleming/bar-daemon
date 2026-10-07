"""Runner contract tests; fake executables never launch desktop services."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("quality-fixtures.sh").resolve()
PIPEWIRE = "audio::tests::private_pipewire_control_reuses_connection_and_reads_external_changes"
IDLE = "native_control_preserves_cookies_and_base_listeners_and_cancels_activity"


class FixtureRunnerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.out = self.root / "evidence"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        HYPRIDLE_TEST_BIN=str(self.bin / "hypridle"),
                        QUALITY_OUTPUT_DIR=str(self.out))
        for name in ["pipewire", "dbus-daemon", "hypridle", "rustc"]:
            self.executable(name, "exit 0\n")

    def executable(self, name, body):
        path = self.bin / name
        path.write_text("#!/usr/bin/env bash\nset -eu\n" + body)
        path.chmod(0o755)

    def cargo(self, results, status=0):
        self.executable("cargo", f'''if [[ "$1" == --version ]]; then exit 0; fi
printf '%s\\n' "$@" > "{self.root}/args"
previous=''
for arg in "$@"; do
  if [[ "$previous" == --output-path ]]; then echo '<coverage/>' > "$arg"; fi
  previous="$arg"
done
cat <<'RESULTS'
{results}
RESULTS
exit {status}
''')

    def run_script(self, mode="test"):
        return subprocess.run(["bash", str(SCRIPT), mode], env=self.env,
                              text=True, capture_output=True)

    def passing(self):
        return f"test {PIPEWIRE} ... ok\ntest {IDLE} ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;"

    def test_full_run_and_fresh_output(self):
        self.cargo(self.passing())
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.out / "complete.txt").exists())
        self.assertEqual((self.root / "args").read_text().splitlines(),
                         ["test", "--workspace", "--all-targets", "--locked", "--", "--include-ignored"])
        self.assertNotEqual(self.run_script().returncode, 0)

    def test_coverage_uses_same_fixture_selection(self):
        self.cargo(self.passing())
        result = self.run_script("coverage")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.out / "cobertura.xml").is_file())
        self.assertIn("--include-ignored", (self.root / "args").read_text())

    def test_missing_prerequisite_does_not_execute_cargo(self):
        self.cargo(self.passing())
        del self.env["HYPRIDLE_TEST_BIN"]
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse((self.root / "args").exists())

    def test_missing_fixture_is_not_success(self):
        self.cargo(f"test {PIPEWIRE} ... ok")
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse((self.out / "complete.txt").exists())

    def test_ignored_test_is_not_success(self):
        self.cargo(self.passing() + "\ntest result: ok. 0 passed; 0 failed; 1 ignored;")
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse((self.out / "complete.txt").exists())

    def test_cargo_failure_survives_tee(self):
        self.cargo(self.passing(), status=7)
        self.assertEqual(self.run_script().returncode, 7)
        self.assertFalse((self.out / "complete.txt").exists())


if __name__ == "__main__":
    unittest.main()
