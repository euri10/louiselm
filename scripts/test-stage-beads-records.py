#!/usr/bin/env python3
"""Offline tests for staging named Beads records (louiselm-ha7ic)."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "stage-beads-records"
JSONL = ".beads/issues.jsonl"


def line(issue_id, title="t"):
    return json.dumps({"id": issue_id, "title": title})


class StageBeadsRecords(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "-q", "--initial-branch=main")
        (self.root / ".beads").mkdir()

    def tearDown(self):
        self.directory.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def commit(self, *lines):
        (self.root / JSONL).write_text("".join(item + "\n" for item in lines))
        self.git("add", JSONL)
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "fixture")

    def work(self, *lines):
        (self.root / JSONL).write_text("".join(item + "\n" for item in lines))

    def run_script(self, *args):
        return subprocess.run([str(SCRIPT), *args], cwd=self.root, env=self.env,
                              capture_output=True, text=True)

    def staged(self):
        return [json.loads(item) for item in self.git("show", f":{JSONL}").splitlines()]

    def test_stages_only_named_records_on_a_sorted_base(self):
        self.commit(line("a"), line("c", "old"), line("d", "committed"))
        self.work(line("a", "other session"), line("b"), line("c", "new"), line("d", "committed"))
        result = self.run_script("b", "c")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.staged(), [{"id": "a", "title": "t"}, {"id": "b", "title": "t"},
                                         {"id": "c", "title": "new"}, {"id": "d", "title": "committed"}])
        self.assertIn("a", self.git("diff", JSONL), "the unnamed working change stays unstaged")

    def test_repeated_calls_accumulate_in_the_index(self):
        self.commit(line("a"))
        self.work(line("a"), line("b"), line("c"))
        self.assertEqual(self.run_script("b").returncode, 0)
        self.assertEqual(self.run_script("c").returncode, 0)
        self.assertEqual([item["id"] for item in self.staged()], ["a", "b", "c"])

    def test_without_ids_sorts_the_base_without_changing_records(self):
        self.commit(line("b"), line("a", "committed"))
        self.work(line("a", "newer"), line("b"))
        self.assertEqual(self.run_script().returncode, 0)
        self.assertEqual(self.staged(), [{"id": "a", "title": "committed"}, {"id": "b", "title": "t"}])

    def test_refuses_an_id_missing_from_the_working_file(self):
        self.commit(line("a"))
        self.work(line("a"))
        before = self.git("show", f":{JSONL}")
        result = self.run_script("zz")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("zz", result.stderr)
        self.assertEqual(self.git("show", f":{JSONL}"), before)

    def test_check_rejects_unsorted_duplicate_and_malformed_records(self):
        for lines, message in [((line("b"), line("a")), "not sorted"),
                               ((line("a"), line("a")), "duplicate"),
                               (("{not json",), "line 1"),
                               (('{"title": "no id"}',), "line 1")]:
            with self.subTest(message=message):
                self.commit(*lines)
                result = self.run_script("--check", "HEAD")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
        self.commit(line("a"), line("b"))
        self.assertEqual(self.run_script("--check", "HEAD").returncode, 0)


if __name__ == "__main__":
    unittest.main()
