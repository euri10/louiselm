#!/usr/bin/env python3
"""Offline gate for the launcher's deliberate-only package update policy."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


POLICY = Path(__file__).with_name("launcher-guest-updates")
UNITS = ("unattended-upgrades.service", "apt-daily.timer", "apt-daily-upgrade.timer",
         "apt-daily.service", "apt-daily-upgrade.service")


class UpdatePolicy(unittest.TestCase):
    def test_disable_and_check_reject_every_enabled_or_running_unit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            systemctl = root / "systemctl"
            systemctl.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
import sys
root = Path(os.environ["UPDATE_FIXTURE"])
verb, *units = sys.argv[1:]
if verb == "mask" and os.environ.get("UPDATE_FAIL_MASK"):
    sys.exit(17)
if verb in ("disable", "mask"):
    for unit in units:
        if unit == "--now":
            continue
        (root / unit).write_text("masked\\ninactive")
elif verb in ("is-enabled", "is-active"):
    print((root / units[0]).read_text().splitlines()[verb == "is-active"])
else:
    sys.exit(99)
''')
            systemctl.chmod(0o700)
            (root / "id").write_text("#!/bin/sh\necho 0\n")
            (root / "id").chmod(0o700)
            env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}", "UPDATE_FIXTURE": directory}

            def run(action):
                return subprocess.run(["sh", str(POLICY), action], env=env,
                                      capture_output=True, text=True, check=False)

            for unit in UNITS:
                (root / unit).write_text("enabled\nactive")
            result = run("disable")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(run("check").returncode, 0)
            for unit in UNITS:
                for state in ("enabled\ninactive", "disabled\nactive", "static\ninactive",
                              "unknown\ninactive", "masked\nactivating"):
                    (root / unit).write_text(state)
                    result = run("check")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(unit, result.stderr)
                    self.assertIn("automatic package updates", result.stderr)
                    (root / unit).write_text("masked\ninactive")
            failed = subprocess.run(["sh", str(POLICY), "disable"],
                                    env={**env, "UPDATE_FAIL_MASK": "1"},
                                    capture_output=True, text=True, check=False)
            self.assertEqual(failed.returncode, 17, "mask failure must not become a passing check")


if __name__ == "__main__":
    unittest.main()
