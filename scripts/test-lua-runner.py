#!/usr/bin/env python3
"""Exercise the real Lua test entrypoint with isolated MiniTest fixtures."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "scripts/test-lua"
PASS = '''local T = MiniTest.new_set()
T["waits for a scheduled result"] = function()
  local done = false
  vim.defer_fn(function() done = true end, 25)
  assert(vim.wait(2000, function() return done end, 5), "timer did not fire")
end
T["finishes the next case"] = function()
  vim.fn.writefile({ "completed" }, "completed")
end
return T
'''
FAIL = '''local T = MiniTest.new_set()
T["fails"] = function() MiniTest.expect.equality(4, 5) end
return T
'''


class TestLuaRunner(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="louiselm-test-lua-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "tests").mkdir()
        shutil.copyfile(ROOT / "tests/minimal_init.lua", self.root / "tests/minimal_init.lua")
        (self.root / "lua").symlink_to(ROOT / "lua", target_is_directory=True)
        self.env = os.environ.copy()
        self.env["MINI_NVIM_PATH"] = str(Path(self.env.get("MINI_NVIM_PATH", ROOT / ".deps/mini.nvim")).resolve())

    def run_fixture(self, file=None):
        args = [str(RUNNER)]
        if file is not None:
            args.append(file)
        result = subprocess.run(args, cwd=self.root, env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=20)
        output = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout)
        return result.returncode, output

    def test_suite_and_file_report_collection_execution_and_empty_results(self):
        fixture = self.root / "tests/fixture_spec.lua"
        for file in (None, "tests/fixture_spec.lua"):
            for source, expected, diagnostic in (
                ('error("synthetic collection failure")\n', 1, "synthetic collection failure"),
                (FAIL, 1, "Failed expectation for equality"),
                ("return MiniTest.new_set()\n", 1, "No cases to execute"),
                (PASS, 0, "Fails (0) and Notes (0)"),
            ):
                with self.subTest(file=file, diagnostic=diagnostic):
                    fixture.write_text(source)
                    (self.root / "completed").unlink(missing_ok=True)
                    status, output = self.run_fixture(file)
                    self.assertEqual(status, expected, output)
                    self.assertIn(diagnostic, output)
                    if source == PASS:
                        self.assertIn("Total number of cases: 2", output)
                        self.assertEqual((self.root / "completed").read_text(), "completed\n")

    def test_focused_file_ignores_other_files_and_preserves_path(self):
        (self.root / "tests/broken_spec.lua").write_text('error("must not be collected")\n')
        focused = "tests/focus 'quoted'_spec.lua"
        (self.root / focused).write_text(PASS)
        status, output = self.run_fixture(focused)
        self.assertEqual(status, 0, output)
        self.assertIn("Total number of cases: 2", output)
        self.assertIn("Fails (0) and Notes (0)", output)
        self.assertNotIn("must not be collected", output)
        self.assertTrue((self.root / "completed").is_file())


if __name__ == "__main__":
    unittest.main()
