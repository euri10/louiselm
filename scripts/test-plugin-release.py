#!/usr/bin/env python3
"""Publication API fixtures: no credentials or network required."""

import base64
import copy
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True

def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("publish-plugin-release")
policy = load("check-release-commits")
SHA = "a" * 40
HEAD = "b" * 40
TAG = "plugin-v0.1.0"


class ApiCredentials(unittest.TestCase):
    def test_administration_token_is_used_only_for_settings_read(self):
        settings = "repos/euri10/louiselm/immutable-releases"
        with patch.dict(os.environ, {"GH_TOKEN": "publication-fixture", "GH_IMMUTABILITY_TOKEN": "settings-fixture"}):
            for path, data, token in [
                (settings, None, "settings-fixture"),
                ("repos/euri10/louiselm/actions/runs/10", None, "publication-fixture"),
                ("repos/euri10/louiselm/releases/7", {"draft": False}, "publication-fixture"),
                (settings, {"enabled": False}, "publication-fixture"),
                ("repos/other/repo/immutable-releases", None, "publication-fixture"),
            ]:
                with self.subTest(path=path, data=data), patch.object(release.subprocess, "run") as run:
                    run.return_value.stdout = '{"ok":true}'
                    self.assertEqual(release.github_api(path, data), {"ok": True})
                    effective_env = run.call_args.kwargs.get("env") or os.environ
                    self.assertEqual(effective_env["GH_TOKEN"], token)
                    self.assertNotIn("GH_IMMUTABILITY_TOKEN", effective_env)
                    self.assertNotIn(token, run.call_args.args[0])
            self.assertEqual(os.environ["GH_TOKEN"], "publication-fixture")

    def test_missing_settings_token_fails_before_request(self):
        with patch.dict(os.environ, {"GH_TOKEN": "publication-fixture"}, clear=True):
            with patch.object(release.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, "GH_IMMUTABILITY_TOKEN.*Administration.*read"):
                    release.github_api("repos/euri10/louiselm/immutable-releases")
                run.assert_not_called()

    def test_api_failure_names_boundary_without_exposing_response(self):
        with patch.dict(os.environ, {"GH_IMMUTABILITY_TOKEN": "settings-fixture"}):
            failure = subprocess.CalledProcessError(1, ["gh", "api"], stderr="private response fixture")
            with patch.object(release.subprocess, "run", side_effect=failure):
                with self.assertRaisesRegex(RuntimeError, "Administration.*read") as error:
                    release.github_api("repos/euri10/louiselm/immutable-releases")
                self.assertNotIn("private response fixture", str(error.exception))
                self.assertNotIn("settings-fixture", str(error.exception))


class Publication(unittest.TestCase):
    def setUp(self):
        self.writes = []
        self.draft = dict(id=7, tag_name=TAG, target_commitish=SHA, draft=True,
                          immutable=False, assets=[], body="## 0.1.0\n\nFeatures", html_url="https://fixture/release")
        self.run = dict(id=10, workflow_id=3, head_sha=SHA, head_branch="main",
                        event="push", status="completed", conclusion="success",
                        repository={"full_name": "euri10/louiselm"}, head_repository={"full_name": "euri10/louiselm"})
        self.pr = dict(number=4, merged=True, merge_commit_sha=SHA, merged_by={"type": "User"},
                       base={"ref": "main", "repo": {"full_name": "euri10/louiselm"}},
                       head={"sha": HEAD, "ref": "release-please--branches--main--components--plugin",
                             "repo": {"full_name": "euri10/louiselm"}},
                       labels=[{"name": "autorelease: tagged"}])
        self.pr_run = dict(self.run, head_sha=HEAD, event="pull_request")
        self.immutable = True
        self.refs = []
        self.version = "0.1.0"

    def api(self, path, data=None):
        if path == "repos/euri10/louiselm": return {"private": False}
        path = path.removeprefix("repos/euri10/louiselm/")
        if data is not None:
            self.writes.append((path, data))
            assert path == "releases/7" and data == {"draft": False, "make_latest": "false"}
            self.draft.update(draft=False, immutable=True)
            self.refs = [{"ref": "refs/tags/" + TAG, "object": {"type": "commit", "sha": SHA}}]
            return copy.deepcopy(self.draft)
        if path == "actions/runs/10": return self.run
        if path == "actions/workflows/ci.yml": return {"id": 3}
        if path == "immutable-releases": return {"enabled": self.immutable}
        if path == "releases?per_page=100": return [copy.deepcopy(self.draft)]
        if path == f"commits/{SHA}/pulls?per_page=100": return [{"number": 4}]
        if path == "pulls/4": return self.pr
        if path.startswith("actions/workflows/ci.yml/runs?"): return {"workflow_runs": [self.pr_run]}
        if path == "git/matching-refs/tags/" + TAG: return self.refs
        if path.startswith("contents/"):
            filename = path.removeprefix("contents/").split("?")[0]
            value = {
                "VERSION": self.version + "\n",
                ".release-please-manifest.json": json.dumps({".": "0.1.0"}),
                "lua/louiselm/version.lua": '-- Generated by scripts/generate-plugin-version; do not edit.\nreturn { version = "0.1.0" } -- x-release-please-version\n',
                "CHANGELOG.md": "# Plugin changelog\n\n## 0.1.0\n",
            }[filename]
            return {"encoding": "base64", "content": base64.b64encode(value.encode()).decode()}
        raise AssertionError(path)

    def publish(self):
        return release.publish(self.api, "euri10/louiselm", 10)

    def test_public_repository_cli_preserves_release_authority(self):
        with patch.object(release, "github_api", self.api), contextlib.redirect_stdout(io.StringIO()) as output:
            with patch.object(sys, "argv", ["publish", "--repository", "euri10/louiselm", "--run-id", "10"]):
                release.main()
            self.assertEqual(json.loads(output.getvalue())[0]["tag"], TAG)
            self.assertEqual(len(self.writes), 1)
            with patch.object(sys, "argv", ["publish", "--repository", "other/repository", "--run-id", "10"]):
                with self.assertRaisesRegex(ValueError, "release authority"):
                    release.main()
            self.assertEqual(len(self.writes), 1)

    def test_publish_then_retry_never_changes_published_bytes_or_tag(self):
        self.publish()
        self.assertEqual(len(self.writes), 1)
        original = copy.deepcopy(self.draft)
        self.publish()
        self.assertEqual(len(self.writes), 1)
        self.assertEqual(self.draft, original)

    def test_failures_remain_unpublished(self):
        for mutate in [
            lambda: self.run.update(conclusion="failure"),
            lambda: self.run.update(event="pull_request"),
            lambda: self.run.update(head_branch="other"),
            lambda: self.run.update(workflow_id=99),
            lambda: self.pr_run.update(head_sha="c" * 40),
            lambda: self.pr_run.update(conclusion="failure"),
            lambda: self.pr.update(merged_by={"type": "Bot"}),
            lambda: self.pr.update(merge_commit_sha="c" * 40),
            lambda: setattr(self, "immutable", False),
            lambda: setattr(self, "version", "0.2.0"),
            lambda: self.draft.update(assets=[{"name": "partial"}]),
            lambda: self.refs.append({"ref": "refs/tags/" + TAG, "object": {"type": "commit", "sha": "c" * 40}}),
        ]:
            with self.subTest(mutate=mutate):
                self.setUp()
                mutate()
                with self.assertRaises(ValueError): self.publish()
                self.assertEqual(self.writes, [])
                self.assertTrue(self.draft["draft"])

    def test_newer_unchecked_draft_is_not_published_by_older_ci(self):
        self.draft["target_commitish"] = "c" * 40
        self.publish()
        self.assertEqual(self.writes, [])

    def test_non_plugin_draft_is_ignored(self):
        self.draft["tag_name"] = "capture-v0.1.0"
        self.publish()
        self.assertEqual(self.writes, [])


class CapturePublication(unittest.TestCase):
    def setUp(self):
        self.fixture = Publication()
        self.fixture.setUp()
        self.fixture.draft['tag_name'] = 'capture-v0.1.0'
        self.fixture.pr['head']['ref'] = 'release-please--branches--main--components--capture'
        self.prepared = []

    def api(self, path, data=None):
        if '/contents/' in path:
            filename = path.split('/contents/')[1].split('?')[0]
            value = {
                'capture-service/Cargo.toml': '[package]\nname="louiselm-capture"\nversion="0.1.0"\n',
                'capture-service/Cargo.lock': '[[package]]\nname="louiselm-capture"\nversion="0.1.0"\n',
                '.release-please-manifest.json': '{"capture-service":"0.1.0"}',
                'capture-service/CHANGELOG.md': '## 0.1.0\n',
            }[filename]
            return dict(encoding='base64', content=base64.b64encode(value.encode()).decode())
        if '/git/matching-refs/tags/capture-' in path:
            return []
        return self.fixture.api(path, data)

    def prepare(self, repository, draft, sha, version):
        self.prepared.append((repository, sha, version))

    def test_capture_requires_approved_exact_source_before_build_or_publication(self):
        self.fixture.pr_run['conclusion'] = 'failure'
        with self.assertRaisesRegex(ValueError, 'pull_request CI'):
            release.publish(self.api, 'euri10/louiselm', 10, component='capture', prepare_assets=self.prepare)
        self.assertEqual(self.prepared, [])
        self.assertEqual(self.fixture.writes, [])
        self.fixture.pr_run['conclusion'] = 'success'
        result = release.publish(self.api, 'euri10/louiselm', 10, component='capture', prepare_assets=self.prepare)
        self.assertEqual(self.prepared, [('euri10/louiselm', SHA, '0.1.0')])
        self.assertEqual(result[0]['component'], 'capture')

    def test_incomplete_assets_leave_draft_unpublished(self):
        def fail(*args):
            raise ValueError('incomplete assets')
        with self.assertRaisesRegex(ValueError, 'incomplete assets'):
            release.publish(self.api, 'euri10/louiselm', 10, component='capture', prepare_assets=fail)
        self.assertEqual(self.fixture.writes, [])


class CommitPolicy(unittest.TestCase):
    def test_protected_history_exception_does_not_relax_commit_policy(self):
        excluded = json.loads(Path("release-please-config.json").read_text())["packages"]["."]["exclude-paths"]
        with self.assertRaises(ValueError):
            policy.validate("fix(vm): archive Provider disclosure fixture", ["scripts/launcher-vm", ".beads/issues.jsonl"], excluded)
        policy.main()

    def test_root_companion_and_site_files_cannot_drive_releases(self):
        for filename in policy.NON_PLUGIN_FILES:
            policy.validate("build: update tool", [filename], ["site"])
            for message in ["fix: tool", "feat!: tool", "chore: tool\n\nBREAKING CHANGE: upgrade", "chore: tool\n\nRelease-As: 1.0.0"]:
                with self.assertRaises(ValueError):
                    policy.validate(message, [filename, "site/index.md"], ["site"])
        policy.validate("feat: plugin and site", ["lua/x.lua", "package.json"], [])
        policy.validate("feat: site", ["site/index.md"], ["site"])

    def test_disposable_history_checks_actual_commit_messages_and_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.check_output([
                    "git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                    "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *args,
                ], cwd=root, text=True, stderr=subprocess.DEVNULL).strip()

            git("init", "--quiet")
            (root / "package.json").write_text('{"legacy":true}')
            git("add", "package.json")
            git("commit", "--quiet", "-m", "feat: legacy tooling before policy")
            baseline = git("rev-parse", "HEAD")
            (root / "release-please-config.json").write_text(json.dumps({
                "packages": {".": {"exclude-paths": ["site"]}},
            }))
            (root / "package.json").write_text("{}")
            git("add", "package.json")
            git("commit", "--quiet", "-m", "build: site tooling")
            script = str(Path(policy.__file__).resolve())
            command = [
                sys.executable, "-c",
                "import runpy, sys; runpy.run_path(sys.argv[1])['main'](sys.argv[2])",
                script, baseline,
            ]
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("1 commits checked", result.stdout)
            (root / "package.json").write_text('{"private":true}')
            git("add", "package.json")
            git("commit", "--quiet", "-m", "feat: site tooling")
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(git("rev-parse", "HEAD"), result.stderr)


class VersionProjection(unittest.TestCase):
    def test_stale_projection_and_unapproved_major_fail_the_real_check(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "lua/louiselm").mkdir(parents=True)
            generator = root / "scripts/generate-plugin-version"
            shutil.copy(Path(__file__).with_name("generate-plugin-version"), generator)
            for version in ["0.7.2", "1.0.0"]:
                (root / "VERSION").write_text(version + "\n")
                (root / ".release-please-manifest.json").write_text(json.dumps({".": version}))
                result = subprocess.run([str(generator)], capture_output=True)
                self.assertEqual(result.returncode == 0, version == "0.7.2")
                if version == "0.7.2":
                    self.assertEqual(subprocess.run([str(generator), "--check"], capture_output=True).returncode, 0)
                    (root / "lua/louiselm/version.lua").write_text('return { version = "0.7.1" }\n')
                    self.assertNotEqual(subprocess.run([str(generator), "--check"], capture_output=True).returncode, 0)


if __name__ == "__main__":
    unittest.main()
