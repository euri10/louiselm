#!/usr/bin/env python3
"""Exercise pinned Lua fixture installation without Git/network access."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().with_name("install-test-deps")
PINS = {
    "mini.nvim": "1345d191bb3da9c7b0e977f4387c5761f9bff68d",
    "snacks.nvim": "882c996cf28183f4d63640de0b4c02ec886d01f2",
}
FAKE_GIT = """#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["TEST_GIT_LOG"], "a") as log:
    log.write(json.dumps(args) + "\\n")
if args[0] == "clone":
    pathlib.Path(args[-1], ".git").mkdir(parents=True)
    sys.exit(0)
assert args[0] == "-C", args
root = pathlib.Path(args[1])
command = args[2:]
if command[0] == "cat-file":
    sys.exit(0 if (root / "available").exists() else 1)
elif command[0] == "fetch":
    (root / "available").touch()
elif command[0] == "checkout":
    (root / "revision").write_text(command[-1])
elif command == ["rev-parse", "HEAD"]:
    print("wrong" if os.environ.get("TEST_WRONG_REVISION") == root.name
          else (root / "revision").read_text())
else:
    raise AssertionError(command)
"""


class InstallerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="louiselm-test-deps-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.root / "scripts/install-test-deps")
        (self.root / "bin").mkdir()
        git = self.root / "bin/git"
        git.write_text(FAKE_GIT)
        git.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.root / 'bin'}:{os.environ['PATH']}",
                        TEST_GIT_LOG=str(self.root / "git.log"))

    def install(self):
        return subprocess.run(["sh", str(self.root / "scripts/install-test-deps")],
                              env=self.env, text=True, capture_output=True, check=False)

    def calls(self):
        return [json.loads(line) for line in (self.root / "git.log").read_text().splitlines()]

    def test_installs_and_verifies_both_exact_revisions(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        for name, revision in PINS.items():
            root = self.root / ".deps" / name
            self.assertTrue(root.is_dir(), f"{name} was not installed")
            self.assertEqual((root / "revision").read_text(), revision)
            self.assertIn(["-C", str(root), "fetch", "--depth", "1", "origin", revision], self.calls())
            self.assertIn(["-C", str(root), "rev-parse", "HEAD"], self.calls())

    def test_cached_revisions_need_no_clone_or_fetch(self):
        self.assertEqual(self.install().returncode, 0)
        (self.root / "git.log").write_text("")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertFalse(any(call[0] == "clone" or call[2] == "fetch" for call in calls))
        self.assertEqual(sum(call[2:] == ["rev-parse", "HEAD"] for call in calls), len(PINS))

    def test_refuses_existing_non_checkout(self):
        root = self.root / ".deps/snacks.nvim"
        root.mkdir(parents=True)
        sentinel = root / "user-file"
        sentinel.write_text("preserved")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exists but is not a git checkout", result.stderr)
        self.assertEqual(sentinel.read_text(), "preserved")

    def test_refuses_wrong_snacks_revision(self):
        self.env["TEST_WRONG_REVISION"] = "snacks.nvim"
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"expected snacks.nvim {PINS['snacks.nvim']}, got wrong", result.stderr)


if __name__ == "__main__":
    unittest.main()
