#!/usr/bin/env python3
"""Exercise recovery coordinator log checks without a VM or ripgrep."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().with_name("test-consumed-recovery-vm")
LIBRARY = "/var/tmp/louiselm-skills-target/debug/deps/louiselm_skills-fixture"
SUCCESS = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 507 filtered out"
MARKERS = {
    "storage": ["RECOVERY_STORAGE_TRUST_AND_PRESERVATION"],
    "before_reboot": ["RECOVERY_ARMED_REBOOT_REQUIRED"],
    "after_reboot": [
        "RECOVERY_PROVED_HISTORY_RETAINED",
        "RECOVERY_RECEIPTED_HEAD_PRESERVED",
        "RECOVERY_INSTALLED_INSPECTION_VERIFIED",
    ],
}
LAUNCHER = """import json, os, pathlib, shutil, sys
root = pathlib.Path(os.environ["RECOVERY_TEST_ROOT"])
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as stream:
    stream.write(json.dumps(args) + "\\n")
if args[0] == "exec":
    assert "--exact" in args and "--test-threads=1" in args
    phase = next((name for name in ("before_reboot", "after_reboot")
                  if any(arg.endswith("privileged_consumed_recovery_" + name) for arg in args)), "storage")
    print((root / (phase + ".log")).read_text(), end="")
    sys.exit(7 if os.environ.get("RECOVERY_TEST_FAIL") == phase else 0)
elif args[0] == "get":
    assert args[1] == "/var/tmp/louiselm-consumed-recovery-evidence.json"
    shutil.copyfile(root / "proof.json", args[2])
else:
    assert args in (["stop"], ["start"]), args
"""


class RecoveryCoordinatorTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="louiselm-recovery-coordinator-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copyfile(SCRIPT, scripts / SCRIPT.name)
        launcher = scripts / "launcher-vm"
        launcher.write_text(f"#!{sys.executable}\n" + LAUNCHER)
        launcher.chmod(0o755)
        tools = self.root / "bin"
        tools.mkdir()
        for name in ("bash", "dirname", "mktemp", "tee", "grep"):
            (tools / name).symlink_to(shutil.which(name))
        self.assertIsNone(shutil.which("rg", path=str(tools)))
        (self.root / "cache").mkdir()
        (self.root / "proof.json").write_text('{"fixture": true}\n')
        self.env = dict(os.environ, PATH=str(tools), RECOVERY_TEST_ROOT=str(self.root),
                        XDG_CACHE_HOME=str(self.root / "cache"))
        self.logs()

    def logs(self, phase=None, result=SUCCESS, missing=None):
        for name, markers in MARKERS.items():
            lines = [result if name == phase else SUCCESS]
            lines.extend(marker for marker in markers if marker != missing)
            (self.root / (name + ".log")).write_text("\n".join(lines) + "\n")
        (self.root / "calls.jsonl").write_text("")

    def run_gate(self):
        return subprocess.run([str(self.root / "bin/bash"), str(self.root / "scripts" / SCRIPT.name), LIBRARY],
                              env=self.env, text=True, capture_output=True, timeout=5, check=False)

    def calls(self):
        return [json.loads(line)[0] for line in (self.root / "calls.jsonl").read_text().splitlines()]

    def refuses(self, result, phase, code=1):
        self.assertEqual(result.returncode, code, result.stderr)
        self.assertNotIn("Recovery VM passed", result.stdout)
        expected = {"storage": ["exec"], "before_reboot": ["exec", "exec"],
                    "after_reboot": ["exec", "exec", "stop", "start", "exec"]}
        self.assertEqual(self.calls(), expected[phase])

    def test_all_phases_and_evidence_copy_work_without_rg(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), ["exec", "exec", "stop", "start", "exec", "get"])
        evidence = list((self.root / "cache").glob("louiselm-consumed-recovery.*"))
        self.assertEqual(len(evidence), 1)
        self.assertEqual(json.loads((evidence[0] / "proof.json").read_text()), {"fixture": True})

    def test_each_required_marker_is_still_required(self):
        for phase, markers in MARKERS.items():
            for marker in markers:
                with self.subTest(marker=marker):
                    self.logs(missing=marker)
                    self.refuses(self.run_gate(), phase)

    def test_each_phase_requires_exactly_one_passing_test(self):
        for phase in MARKERS:
            for result in ("test result: ok. 0 passed; 0 failed;", "test result: ok. 2 passed; 0 failed;",
                           "test result: ok. 1 passed; 1 failed;", "test result: okX 1 passed; 0 failed;", ""):
                with self.subTest(phase=phase, result=result):
                    self.logs(phase=phase, result=result)
                    self.refuses(self.run_gate(), phase)

    def test_nonzero_phase_exit_refuses_even_with_successful_log(self):
        for phase in MARKERS:
            with self.subTest(phase=phase):
                self.logs()
                self.env["RECOVERY_TEST_FAIL"] = phase
                self.refuses(self.run_gate(), phase, code=7)


if __name__ == "__main__":
    unittest.main()
