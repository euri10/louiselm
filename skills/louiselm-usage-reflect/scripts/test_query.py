"""Exercise the installed query bridge against private synthetic metadata only.

Run: python3 test_query.py /absolute/path/to/louiselm
Seed a private demonstration: append --seed /absolute/scratch/usage
"""
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest


REPO = Path(sys.argv.pop(1)).resolve()
QUERY = Path(__file__).with_name("query.lua")


def seed(directory):
    # Reuse the repository's schema initializer; it touches only this scratch DB.
    subprocess.run([
        "nvim", "--headless", "--noplugin", "-u", "NONE", "-l",
        str(REPO / "docs/performance/usage-20260924/benchmark.lua"),
        str(directory), "seed",
    ], cwd=REPO, check=True, capture_output=True, timeout=20)
    low = {"model": "example", "effort": "low", "enabled": False}
    high = {**low, "effort": "high", "enabled": True}
    with sqlite3.connect(directory / "turns.sqlite3") as db:
        for number, options, provider, usage, outcome in [
            (1, low, "service-a", {"total_tokens": 100, "thought_tokens": 0}, "completed"),
            (2, high, "service-a", {"total_tokens": 40}, "failed"),
            (3, high, "service-a", None, "cancelled"),
            (4, high, "service-a", {"total_tokens": 60}, "completed"),
            (5, high, "service-a", None, None),
            (6, {**high, "enabled": "false"}, "service-b", {"total_tokens": 0}, "completed"),
        ]:
            turn_id = f"t{number}"
            stamp = f"2026-09-07T0{number}:00:00Z"
            currency = "EUR" if number == 6 else "USD"
            baseline = {"amount": 1, "currency": currency} if number in (1, 6) else None
            db.execute("INSERT INTO turns VALUES(?,?,?,?,?,?,?,?)", (
                turn_id, "fixture", provider, "s1" if number < 5 else f"s{number - 3}", stamp, json.dumps(options),
                json.dumps(options["model"]), json.dumps(baseline) if baseline else None,
            ))
            events = [(turn_id, 1, "dispatch", stamp, "{}")]
            if baseline:
                events.append((turn_id, 2, "cost", stamp, json.dumps({
                    "cost": {"amount": 3 if number == 1 else 1, "currency": currency},
                })))
            if outcome:
                data = {"outcome": outcome, "peer_response": True}
                if usage is not None:
                    data["usage"] = usage
                terminal_stamp = "2026-09-07T04:02:00Z" if number == 4 else stamp
                events.append((turn_id, 3, "outcome", terminal_stamp, json.dumps(data)))
            db.executemany("INSERT INTO turn_events VALUES(?,?,?,?,?)", events)
        for change in [
            ("c1", "obs1", 1, "fixture", "s1", "2026-09-07T01:30:00Z",
             json.dumps(low), json.dumps(high), "response",
             json.dumps({"id": "r1", "option": "effort", "value": "high"}), None),
            ("c2", "obs1", 2, "fixture", "s1", "2026-09-07T04:01:00Z",
             json.dumps(high), json.dumps(low), "notification", None, "t4"),
        ]:
            db.execute("INSERT INTO option_events VALUES(?,?,?,?,?,?,?,?,?,?,?)", change)


class QueryTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="usage-reflect-test-")
        self.addCleanup(self.scratch.cleanup)
        self.directory = Path(self.scratch.name) / "usage"
        seed(self.directory)

    def query(self, query, directory=None, expected=0):
        result = subprocess.run([
            "nvim", "--headless", "--noplugin", "-u", "NONE", "-l", str(QUERY),
            str(REPO), str(directory or self.directory),
            query if isinstance(query, str) else json.dumps(query),
        ], capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, expected, result.stderr)
        return json.loads(result.stdout)

    def test_sparse_mixed_coverage_and_read_only(self):
        database = self.directory / "turns.sqlite3"
        before = hashlib.sha256(database.read_bytes()).hexdigest()
        page = self.query({})["page"]
        self.assertEqual(page["summary"]["turns"], 6)
        self.assertEqual(page["summary"]["outcomes"], {
            "completed": 3, "failed": 1, "cancelled": 1, "unobserved": 1,
        })
        self.assertEqual(page["summary"]["tokens"]["total_tokens"], {
            "samples": 4, "average": 50, "total": 200,
        })
        self.assertEqual(page["summary"]["tokens"]["thought_tokens"]["samples"], 1)
        self.assertEqual(page["summary"]["tokens"]["thought_tokens"]["total"], 0)
        self.assertEqual([(x["currency"], x["total"], x["samples"])
                          for x in page["summary"]["costs"]], [("EUR", 0, 1), ("USD", 2, 1)])
        self.assertEqual(page["mixed_turns"], 1)
        page = self.query({"group_by": ["provider", "model", "option:effort"]})["page"]
        self.assertEqual(page["summary"]["turns"], 5)
        self.assertEqual(page["excluded_mixed"], 1)
        cohort = next(row for row in page["rows"] if
                      row["dimensions"].get("provider") == "service-a" and
                      row["dimensions"].get("option:effort") == "high")
        self.assertEqual(cohort["summary"]["turns"], 3)
        self.assertEqual(cohort["summary"]["tokens"]["total_tokens"]["samples"], 1)
        self.assertEqual(hashlib.sha256(database.read_bytes()).hexdigest(), before)

    def test_source_references_idle_changes_and_pagination(self):
        page = self.query({"view": "events", "filters": {"agent": "fixture", "session": "s1"}, "limit": 2})["page"]
        rows = list(page["rows"])
        while page.get("next_offset") is not None:
            page = self.query({"view": "events", "filters": {"agent": "fixture", "session": "s1"},
                               "limit": 2, "offset": page["next_offset"]})["page"]
            rows.extend(page["rows"])
        changes = {row["id"]: row for row in rows if row["kind"] == "options"}
        self.assertEqual(set(changes), {"c1", "c2"})
        self.assertEqual(changes["c1"]["data"]["request"]["option"], "effort")
        self.assertIsNone(changes["c1"].get("turn_id"))
        self.assertIsNone(changes["c2"]["data"].get("request"))
        self.assertEqual(changes["c2"]["turn_id"], "t4")
        page = self.query({"view": "events", "turn_id": "t4"})["page"]
        self.assertIn("c2", [row["id"] for row in page["rows"]])
        self.assertNotIn("c1", [row["id"] for row in page["rows"]])

    def test_typed_filters_empty_missing_and_invalid(self):
        for value, turn_id in [(False, "t1"), ("false", "t6")]:
            page = self.query({"view": "turns", "filters": {"option:enabled": value}})["page"]
            self.assertEqual([row["id"] for row in page["rows"]], [turn_id])
        empty = self.query({"filters": {"agent": "absent"}})
        self.assertTrue(empty["store_present"])
        self.assertEqual(empty["page"]["summary"]["turns"], 0)
        missing = self.directory.parent / "missing"
        result = self.query({}, missing)
        self.assertFalse(result["store_present"])
        self.assertFalse(missing.exists())
        invalid_directory = self.query({}, directory="relative-usage", expected=1)
        self.assertEqual(invalid_directory["error"]["code"], "invalid")
        for query in ['{"sensitive-invalid-json":', {"limit": 101}, {"raw_logs": True}]:
            result = self.query(query, expected=1)
            self.assertEqual(result["error"]["code"], "invalid")
            self.assertNotIn("sensitive-invalid-json", json.dumps(result))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--seed":
        seed(Path(sys.argv[2]))
    else:
        unittest.main()
