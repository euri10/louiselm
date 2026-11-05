#!/usr/bin/env python3
"""Offline tests for staging named Beads records (louiselm-ha7ic)."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "stage-beads-records"
JSONL = ".beads/issues.jsonl"
HISTORY = ".beads/issues.history.jsonl"
STAMP = "2026-11-05T10:00:00.123456789Z"
# v1 wire layout from beads_rust main@5830c1e, src/storage/portable_history.rs.
COLUMNS = {
    "events": "id issue_id event_type actor old_value new_value comment created_at agent_name harness model",
    "gate_result_history": "id issue_id from_status to_status status_revision gate provider passed note recorded_by recorded_at",
    "capacity_exemption_history": "id issue_id capacity_kind capacity_name action provider reason actor expires_at recorded_at",
    "gate_results": "issue_id gate provider passed note recorded_by recorded_at",
    "close_metadata": "issue_id closed_by_agent_name closed_by_harness closed_by_model bypassed_policy bypass_reason policy_gates_fired recorded_at",
    "capacity_occupancy": "issue_id actor agent_name harness session recorded_at",
    "capacity_exemptions": "issue_id capacity_kind capacity_name provider reason granted_by granted_at expires_at ended_at ended_action ended_by",
}


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def checkpoint(*lines):
    issues = sorted((json.loads(item) for item in lines), key=lambda issue: issue["id"])
    anchors = {issue["id"]: {key: issue[key] for key in ("status", "created_at", "updated_at")}
               for issue in issues}
    tables = [{"table": name, "columns": columns.split(), "rows": []}
              for name, columns in COLUMNS.items()]
    tables[0]["rows"] = [[1, issue["id"], "created", "fixture", None, None,
                          issue["title"], STAMP, None, None, None] for issue in issues]
    return seal({"version": 1, "history": {"issues": anchors, "tables": tables},
                 "warnings": ["Fixture history; no production data."]})


def seal(cp):
    cp["anchor_sha256"] = hashlib.sha256(encoded(cp["history"]["issues"]).encode()).hexdigest()
    cp["history_sha256"] = hashlib.sha256(encoded(
        [cp["version"], cp["anchor_sha256"], cp["history"], cp["warnings"]]).encode()).hexdigest()
    return cp


def line(issue_id, title="t"):
    return json.dumps({"id": issue_id, "title": title, "status": "open",
                       "created_at": STAMP, "updated_at": STAMP})


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

    def commit(self, *lines, history=True):
        (self.root / JSONL).write_text("".join(item + "\n" for item in lines))
        self.git("add", JSONL)
        if history:
            valid = []
            for item in lines:
                try:
                    if "id" in json.loads(item):
                        valid.append(item)
                except ValueError:
                    pass  # The issue-parser refusal fixtures intentionally contain bad rows.
            self.history(checkpoint(*{json.loads(item)["id"]: item for item in valid}.values()))
            self.git("add", HISTORY)
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "fixture")

    def work(self, *lines):
        (self.root / JSONL).write_text("".join(item + "\n" for item in lines))
        self.history(checkpoint(*lines))

    def history(self, *checkpoints):
        (self.root / HISTORY).write_text("".join(encoded(cp) + "\n" for cp in checkpoints))

    def run_script(self, *args):
        return subprocess.run([str(SCRIPT), *args], cwd=self.root, env=self.env,
                              capture_output=True, text=True)

    def staged(self):
        return [{key: json.loads(item)[key] for key in ("id", "title")}
                for item in self.git("show", f":{JSONL}").splitlines()]

    def staged_history(self):
        return json.loads(self.git("show", f":{HISTORY}").strip())

    def test_selects_history_without_other_sessions_even_at_the_same_anchor(self):
        self.commit(line("a", "committed a"), line("b", "committed b"))
        self.work(line("a", "other Session"), line("b", "our Session — é 🦀"), line("c", "unselected"))
        before = [(self.root / name).read_bytes() for name in (JSONL, HISTORY)]
        result = self.run_script("b")
        self.assertEqual(result.returncode, 0, result.stderr)
        cp = self.staged_history()
        self.assertEqual(list(cp["history"]["issues"]), ["a", "b"])
        self.assertEqual([row[6] for row in cp["history"]["tables"][0]["rows"]],
                         ["committed a", "our Session — é 🦀"])
        self.assertEqual(before, [(self.root / name).read_bytes() for name in (JSONL, HISTORY)])
        self.assertEqual(self.run_script("--check").returncode, 0)

    def test_enrolls_only_selected_legacy_history_and_reports_the_gap(self):
        self.commit(line("a", "committed a"), history=False)
        self.work(line("a", "other Session"), line("b", "our Session"))
        result = self.run_script("b")
        self.assertEqual(result.returncode, 0, result.stderr)
        cp = self.staged_history()
        self.assertEqual(list(cp["history"]["issues"]), ["a", "b"])
        self.assertEqual([row[1] for row in cp["history"]["tables"][0]["rows"]], ["b"])
        self.assertTrue(any("missing" in warning.lower() for warning in cp["warnings"]))

    def test_uses_matching_older_generation_and_keeps_nanoseconds(self):
        self.commit(line("a"))
        self.work(line("a", "selected"))
        matching = checkpoint(line("a", "selected"))
        newer = checkpoint(line("a", "not yet exported"))
        newer["history"]["issues"]["a"]["updated_at"] = "2026-11-05T10:00:00.123456790Z"
        self.history(matching, seal(newer))
        result = self.run_script("a")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.staged_history()["history"]["tables"][0]["rows"][0][6], "selected")

    def test_history_refusals_leave_both_index_files_unchanged(self):
        self.commit(line("a"))
        self.work(line("a", "selected"))
        good = checkpoint(line("a", "selected"))
        damaged = json.loads(encoded(good))
        damaged["history_sha256"] = "0" * 64
        stale = json.loads(encoded(good))
        stale["history"]["issues"]["a"]["updated_at"] = "2026-11-05T10:00:00.123456790Z"
        newer_format = json.loads(encoded(good))
        newer_format["version"] = 2
        foreign = json.loads(encoded(good))
        foreign["history"]["tables"][0]["rows"][0][1] = "foreign"
        for cp, diagnostic in [(damaged, "checksum"), (seal(stale), "anchor"),
                               (seal(newer_format), "version"), (seal(foreign), "unknown issue")]:
            with self.subTest(diagnostic=diagnostic):
                self.history(cp)
                before = self.git("ls-files", "--stage")
                result = self.run_script("a")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertEqual(self.git("ls-files", "--stage"), before)

    def test_check_rejects_missing_history_and_anchor_mismatch(self):
        self.commit(line("a"), history=False)
        result = self.run_script("--check", "HEAD")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("history", result.stderr)
        self.commit(line("a"))
        cp = checkpoint(line("a"))
        cp["history"]["issues"]["a"]["updated_at"] = "2026-11-05T11:00:00Z"
        self.history(seal(cp))
        self.git("add", HISTORY)
        result = self.run_script("--check")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("anchor", result.stderr)

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
        self.work(line("a", "other Session"), line("b", "not selected again"), line("c"))
        self.assertEqual(self.run_script("c").returncode, 0)
        self.assertEqual([item["id"] for item in self.staged()], ["a", "b", "c"])
        self.assertEqual([row[6] for row in self.staged_history()["history"]["tables"][0]["rows"]],
                         ["t", "t", "t"])

    def test_selects_every_table_without_renumbering_history(self):
        self.commit(line("a"), line("b"))
        baseline = checkpoint(line("a"), line("b"))
        self.work(line("a", "other"), line("b", "selected"))
        cp = checkpoint(line("a", "other"), line("b", "selected"))
        rows = [
            [1, "", "status_changed", "fixture", "open", "open", None, STAMP, None, None, None],
            [1, "", "open", "closed", 2, "qa", "fixture", 1, None, "fixture", STAMP],
            [1, "", "label", "fixture", "grant", "fixture", None, "fixture", None, STAMP],
            ["", "qa", "fixture", 1, None, "fixture", STAMP],
            ["", None, None, None, 0, None, None, STAMP],
            ["", "fixture", None, None, None, STAMP],
            ["", "label", "fixture", "fixture", "test", "fixture", STAMP, None, None, None, None],
        ]
        for table, template in zip(cp["history"]["tables"], rows):
            owner = table["columns"].index("issue_id")
            table["rows"] = []
            for issue_id in ("a", "b"):
                for sequence in range(1, 12) if owner == 1 else [1]:
                    row = template.copy()
                    row[owner] = issue_id
                    if owner == 1:
                        row[0] = sequence
                    table["rows"].append(row)
        self.history(seal(cp))
        result = self.run_script("b")
        self.assertEqual(result.returncode, 0, result.stderr)
        for old, expected, actual in zip(baseline["history"]["tables"], cp["history"]["tables"],
                                        self.staged_history()["history"]["tables"]):
            owner = expected["columns"].index("issue_id")
            self.assertEqual([row for row in actual["rows"] if row[owner] == "b"],
                             [row for row in expected["rows"] if row[owner] == "b"])
            self.assertEqual([row for row in actual["rows"] if row[owner] == "a"],
                             [row for row in old["rows"] if row[owner] == "a"])

    def test_equivalent_timestamp_offsets_match_without_rounding(self):
        self.commit(line("a"))
        self.work(line("a", "selected"))
        value = json.loads(line("a", "selected"))
        value["updated_at"] = "2026-11-05T11:00:00.123456789+01:00"
        (self.root / JSONL).write_text(encoded(value) + "\n")
        result = self.run_script("a")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_duplicate_keys_generations_and_symlinks_before_staging(self):
        self.commit(line("a"))
        self.work(line("a", "selected"))
        cp = checkpoint(line("a", "selected"))
        for text, diagnostic in [(encoded(cp).replace('"version":1', '"version":1,"version":1'), "duplicate JSON key"),
                                 (encoded(cp) + "\n" + encoded(cp), "duplicate history generation"),
                                 ("", "generations"), ("not JSON", "Expecting value")]:
            with self.subTest(diagnostic=diagnostic):
                (self.root / HISTORY).write_text(text)
                before = self.git("ls-files", "--stage")
                result = self.run_script("a")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertEqual(self.git("ls-files", "--stage"), before)
        target = self.root / "owned-history-fixture"
        (self.root / HISTORY).rename(target)
        (self.root / HISTORY).symlink_to(target)
        result = self.run_script("a")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("regular", result.stderr)
        self.assertEqual(self.git("ls-files", "--stage"), before)

    @unittest.skipUnless(os.environ.get("LOUISELM_TEST_BR"), "set LOUISELM_TEST_BR for native fresh-clone replay")
    def test_native_br_replays_only_selected_history_in_a_fresh_clone(self):
        executable = os.environ["LOUISELM_TEST_BR"]
        actor = os.environ["LOUISELM_TEST_BR_ACTOR"]

        def br(root, *args):
            env = {key: value for key, value in self.env.items()
                   if not key.startswith(("BEADS_", "BD_", "BR_"))}
            env.update(BEADS_DIR=str(root / ".beads"), BEADS_DB=str(root / ".beads/beads.db"),
                       BEADS_JSONL=str(root / JSONL), BEADS_CACHE_DIR=str(root / ".beads"), RUST_LOG="error")
            result = subprocess.run([executable, *args, "--actor", actor, "--json"], cwd=root,
                                    env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads(result.stdout)

        br(self.root, "init", "--prefix", "fixture")
        (self.root / ".beads/policy.yaml").write_text(
            "workflow:\n  strict: true\n  gates:\n    'open -> closed':\n      require_all: [qa]\n")
        first = br(self.root, "create", "baseline-a")["id"]
        second = br(self.root, "create", "baseline-b")["id"]
        br(self.root, "sync", "--flush-only")
        self.git("add", JSONL, HISTORY, ".beads/policy.yaml")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "native baseline")
        br(self.root, "update", first, "--title", "other-native-session")
        br(self.root, "update", second, "--title", "selected-native-session é 🦀")
        br(self.root, "gate", "report", second, "--gate", "qa", "--provider", "fixture",
           "--status", "pass", "--to", "closed")
        br(self.root, "close", second, "--reason", "native replay fixture")
        before = [(self.root / name).read_bytes() for name in (JSONL, HISTORY)]
        result = self.run_script(second)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(before, [(self.root / name).read_bytes() for name in (JSONL, HISTORY)])
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "selected pair")
        clone = self.root / "clone"
        self.git("clone", "--quiet", "--no-hardlinks", str(self.root), str(clone))
        br(clone, "init", "--prefix", "fixture")
        br(clone, "sync", "--import-only")
        self.assertEqual(br(clone, "show", first)[0]["title"], "baseline-a")
        self.assertEqual(br(clone, "show", second)[0]["title"], "selected-native-session é 🦀")
        self.assertEqual(br(clone, "show", second)[0]["closed_by"], actor)
        self.assertNotIn("other-native-session", encoded(br(clone, "audit", "log", first)))
        self.assertIn("selected-native-session", encoded(br(clone, "audit", "log", second)))
        self.assertIn('"qa"', encoded(br(clone, "gate", "list", second)))

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
