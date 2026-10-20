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

    def test_both_modes_require_full_fixtures_and_fresh_output(self):
        self.cargo(self.passing())
        for mode in ["test", "coverage"]:
            with self.subTest(mode=mode):
                self.out = self.root / mode
                self.env["QUALITY_OUTPUT_DIR"] = str(self.out)
                result = self.run_script(mode)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((self.out / "complete.txt").exists())
                args = (self.root / "args").read_text().splitlines()
                expected = ["test"] if mode == "test" else ["llvm-cov"]
                expected += ["--workspace", "--all-targets", "--locked"]
                if mode == "coverage":
                    expected += ["--cobertura", "--output-path", str(self.out / "cobertura.xml")]
                    self.assertTrue((self.out / "cobertura.xml").is_file())
                self.assertEqual(args, expected + ["--", "--include-ignored"])
                self.assertNotEqual(self.run_script(mode).returncode, 0)

if __name__ == "__main__":
    unittest.main()
