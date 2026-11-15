#!/usr/bin/env python3
"""Exercise release synchronization against disposable local Git repositories."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SYNC = Path(__file__).with_name("sync-gitlab-release.py").resolve()


class ReleaseSync(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="release-sync-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "github.git"
        self.destination = self.root / "gitlab.git"
        self.work = self.root / "work"
        self.env = os.environ | {
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_AUTHOR_NAME": "fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "fixture",
            "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        }
        self.git(self.root, "init", "--bare", "--quiet", str(self.source))
        self.git(self.root, "init", "--bare", "--quiet", str(self.destination))
        self.git(self.root, "init", "--quiet", "-b", "main", str(self.work))
        (self.work / ".gitlab-ci.yml").write_text("existing: pipeline\n")
        self.commit("initial")
        self.base = self.git(self.work, "rev-parse", "HEAD")
        self.git(self.work, "push", "--quiet", str(self.destination), "HEAD:refs/heads/main")
        (self.work / "release.txt").write_text("approved\n")
        self.commit("release")
        self.sha = self.git(self.work, "rev-parse", "HEAD")
        self.records = [
            {"component": name, "version": "0.1.0", "tag": f"{name}-v0.1.0", "sha": self.sha, "release_id": i}
            for i, name in enumerate(("plugin", "capture", "skills", "android"), 1)
        ]
        for index, record in enumerate(self.records):
            if index % 2:
                self.git(self.work, "tag", "-a", record["tag"], "-m", "release")
            else:
                self.git(self.work, "tag", record["tag"])
        self.git(self.work, "push", "--quiet", str(self.source), "HEAD:refs/heads/main",
                 *(f"refs/tags/{record['tag']}" for record in self.records))

    def git(self, cwd, *args):
        return subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false",
                               "-c", "core.hooksPath=/dev/null", *args], cwd=cwd, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def commit(self, message):
        self.git(self.work, "add", ".gitlab-ci.yml", *(["release.txt"] if (self.work / "release.txt").exists() else []))
        self.git(self.work, "commit", "--quiet", "-m", message)

    def refs(self, repo):
        result = self.git(repo, "for-each-ref", "--format=%(refname) %(objectname)")
        return dict(line.split(" ", 1) for line in result.splitlines())

    def sync(self, records=None, source=None, destination=None):
        return subprocess.run(
            [sys.executable, str(SYNC), "--source", str(source or self.source),
             "--destination", str(destination or self.destination)],
            input=json.dumps(self.records if records is None else records), cwd=self.work,
            env=self.env, text=True, capture_output=True,
        )

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)

    def assert_refused(self, result, reason, before):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(reason, result.stderr)
        self.assertEqual(self.refs(self.destination), before)

    def test_four_metadata_tags_transfer_exact_objects_and_ancestry(self):
        self.git(self.work, "push", "--quiet", str(self.destination), f"{self.base}:refs/heads/gitlab-only")
        before = self.refs(self.destination)
        missing = subprocess.run(["git", "cat-file", "-e", self.sha], cwd=self.destination,
                                 env=self.env, capture_output=True)
        self.assertNotEqual(missing.returncode, 0)
        self.assert_success(self.sync())
        refs = self.refs(self.destination)
        self.assertEqual(refs["refs/heads/main"], self.sha)
        self.assertEqual(refs["refs/heads/gitlab-only"], before["refs/heads/gitlab-only"])
        for record in self.records:
            ref = "refs/tags/" + record["tag"]
            self.assertEqual(refs[ref], self.refs(self.source)[ref])
        self.assertEqual(self.git(self.destination, "show", "refs/heads/main:.gitlab-ci.yml"), "existing: pipeline")
        self.git(self.destination, "merge-base", "--is-ancestor", self.base, self.sha)

    def test_actual_publisher_records_drive_synchronization(self):
        spec = importlib.util.spec_from_file_location("publication_fixture", SYNC.with_name("test-plugin-release.py"))
        publication = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(publication)
        publication.SHA = self.sha
        fixture = publication.Publication()
        fixture.setUp()
        records = fixture.publish()
        self.assertEqual(records, [self.records[0] | {"release_id": 7}])
        self.assert_success(self.sync(records))
        self.assertEqual(self.refs(self.destination)["refs/tags/plugin-v0.1.0"],
                         self.refs(self.source)["refs/tags/plugin-v0.1.0"])

    def test_repeated_and_interrupted_sync_are_idempotent(self):
        self.git(self.work, "push", "--quiet", str(self.destination),
                 "HEAD:refs/heads/main", "refs/tags/plugin-v0.1.0")
        self.assert_success(self.sync())
        completed = self.refs(self.destination)
        self.assert_success(self.sync())
        self.assertEqual(self.refs(self.destination), completed)

    def test_destination_ahead_of_release_keeps_its_gitlab_only_commit(self):
        (self.work / "release.txt").write_text("gitlab-only\n")
        self.commit("gitlab-only")
        ahead = self.git(self.work, "rev-parse", "HEAD")
        self.git(self.work, "push", "--quiet", str(self.destination), "HEAD:refs/heads/main")
        self.assert_success(self.sync())
        self.assertEqual(self.refs(self.destination)["refs/heads/main"], ahead)
        self.assertEqual(len(self.refs(self.destination)), 5)

    def test_diverged_main_refuses_before_any_update(self):
        self.git(self.work, "checkout", "--quiet", "-b", "gitlab-divergence", self.base)
        (self.work / "release.txt").write_text("unrelated\n")
        self.commit("gitlab-only")
        self.git(self.work, "push", "--quiet", str(self.destination), "HEAD:refs/heads/main")
        before = self.refs(self.destination)
        self.assert_refused(self.sync(), "diverged", before)

    def test_conflicting_tag_refuses_before_branch_or_other_tag_updates(self):
        # A tag object with the same peeled commit is still a different identity.
        self.git(self.work, "tag", "-a", "conflict", self.sha, "-m", "different tag object")
        self.git(self.work, "push", "--quiet", str(self.destination), "refs/tags/conflict:refs/tags/plugin-v0.1.0")
        before = self.refs(self.destination)
        self.assert_refused(self.sync(), "conflicting tag", before)

    def test_bad_or_unavailable_metadata_and_source_never_update_destination(self):
        before = self.refs(self.destination)
        for records, reason in [
            ([self.records[0] | {"tag": "capture-v0.1.0"}], "metadata"),
            ([self.records[0] | {"sha": "a" * 40}], "source tag"),
            ([self.records[0], self.records[0]], "metadata"),
            ([self.records[0] | {"version": "1.0.0"}], "metadata"),
        ]:
            with self.subTest(records=records):
                self.assert_refused(self.sync(records), reason, before)
        result = self.sync(source=self.root / "missing-secret-fixture")
        self.assert_refused(result, "source", before)
        self.assertNotIn("missing-secret-fixture", result.stderr)
        result = self.sync(destination=self.root / "missing-secret-fixture")
        self.assert_refused(result, "credentials", before)
        self.assertNotIn("missing-secret-fixture", result.stderr)
        self.assert_success(self.sync([], source=self.root / "missing", destination=self.root / "missing"))
        self.assertEqual(self.refs(self.destination), before)

    def test_interrupted_after_tags_repairs_only_main(self):
        self.git(self.work, "push", "--quiet", str(self.destination),
                 *(f"refs/tags/{record['tag']}" for record in self.records))
        self.assert_success(self.sync())
        refs = self.refs(self.destination)
        self.assertEqual(refs["refs/heads/main"], self.sha)
        self.assertEqual(len(refs), 5)

    def test_atomic_rejection_leaves_branch_and_tags_unchanged(self):
        hook = self.destination / "hooks" / "pre-receive"
        hook.write_text("#!/bin/sh\nexit 1\n")
        hook.chmod(0o755)
        before = self.refs(self.destination)
        self.assert_refused(self.sync(), "rejected", before)


if __name__ == "__main__":
    unittest.main()
