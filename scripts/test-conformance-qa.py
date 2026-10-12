#!/usr/bin/env python3
"""Offline bounds/uncertainty tests, never desktop acceptance."""
import copy
import importlib.util
import tempfile
import unittest
import sys
import io
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("conformance_qa", ROOT / "scripts/conformance-qa.py")
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


def inputs():
    manifest = {
        "schema": "louiselm.session.input-manifest/3",
        "agent": {"id": "qa", "tool_integration": "louiselm.test-tool-integration/1"},
        "skills": {"mode": "off", "view_digest": qa.EMPTY_VIEW},
        "isolation": {"network": "denied"},
        "envelope": {"id": "qa", "revision": 1},
        "tool_schemas": [], "plugin_schemas": [], "acp_mcp_servers": [],
    }
    proposal = {"schema": "louiselm.run.input-proposal/1", "run_id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
                "envelope": manifest["envelope"], "manifest_digest": qa.digest(qa.encoded(manifest)),
                "base_commit": "a" * 40}
    return proposal, manifest


class Bounds(unittest.TestCase):
    def test_real_proposal_selects_only_exact_finite_denied_authority(self):
        proposal, manifest = inputs()
        record = qa.records(proposal, manifest, 1000, 1_000)
        envelope = record["envelope"]
        self.assertEqual(envelope["expires_at_ms"], 3_601_000)
        self.assertEqual(envelope["max_sessions"], 12)
        self.assertIsNone(envelope["bead_scope"])
        self.assertIsNone(envelope["provider_requests"])
        self.assertEqual(envelope["commands"]["command_digest"], qa.digest(b"printf QA_OK"))
        self.assertEqual(len(record["grants"]), 12)
        self.assertEqual(len({g["request"]["session_id"] for g in record["grants"]}), 12)
        for grant in record["grants"]:
            for family in ("beads_mutations", "provider_requests", "dependencies", "skill_requests"):
                self.assertIsNone(grant[family])
            self.assertEqual(grant["commands"]["uses"], 2)
            self.assertEqual(grant["commands"]["timeout_ms"], 5000)
            self.assertFalse(grant["commands"]["allow_delegation"])
            self.assertEqual(grant["expires_at_ms"], envelope["expires_at_ms"])

    def test_substituted_enabled_or_networked_inputs_refuse(self):
        proposal, manifest = inputs()
        for field, value in (("skills", {"mode": "enabled"}), ("isolation", {"network": "brokered"}),
                             ("plugin_schemas", [{}])):
            changed = copy.deepcopy(manifest)
            changed[field] = value
            with self.assertRaises(ValueError):
                qa.records(proposal, changed, 1000, 1000)

    def test_uncertain_delivery_blocks_next_across_restart(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "journal.json"
            journal = qa.Journal.create(path)
            self.assertEqual(journal.reserve(1000, 2000), 0)
            with self.assertRaises(ValueError):
                qa.Journal.open(path).reserve(1000, 2000)
            with self.assertRaises(ValueError):
                journal.terminal({"payload": {"resulting_state": "running"}})
            self.assertEqual(qa.Journal.open(path).value["active"], 0)

    def test_original_expiry_and_twelve_authorizations_are_not_renewed(self):
        with tempfile.TemporaryDirectory() as temporary:
            journal = qa.Journal.create(Path(temporary) / "journal.json")
            with self.assertRaises(ValueError):
                journal.reserve(2000, 2000)
            for index in range(12):
                self.assertEqual(journal.reserve(1000, 2000), index)
                journal.terminal({"payload": {"resulting_state": "terminal", "outcome": {"action": "disposal"}}})
            with self.assertRaises(ValueError):
                journal.reserve(1000, 2000)

    def test_command_spending_is_persisted_before_delivery_and_resume_never_resets_it(self):
        proposal, manifest = inputs()
        request = qa.records(proposal, manifest, 1000, 1000)["grants"][0]["request"]
        reply = {"session_id": request["session_id"], "run_id": request["run_id"], "envelope_revision": 1,
                 "request_id": "qa-command-1", "operation": {"kind": "result", "outcome": {
                     "kind": "completed", "output": {"exit_code": 0, "stdout": "QA_OK", "stderr": "",
                                                        "truncated": False, "timed_out": False}}}}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "journal.json"
            journal = qa.Journal.create(path)
            journal.reserve(1000, 10**15)
            journal.value["pending"] = None
            journal.save()
            process = SimpleNamespace(stdin=io.BytesIO(), stdout=io.BytesIO())
            def delivered(_stream, _deadline):
                stored = qa.Journal.open(path).value
                self.assertEqual(stored["spent"], 1)
                self.assertEqual(stored["pending"]["request_id"], "qa-command-1")
                return b"\x1e" + qa.encoded(reply)
            with patch.object(qa, "status_for", return_value={"state": "running"}), patch.object(qa, "read_line", side_effect=delivered):
                qa.command(journal, process, request, 10**15)
            reopened = qa.Journal.open(path)
            self.assertEqual(reopened.value["spent"], 1)
            with patch.object(qa, "status_for", return_value={"state": "running"}), patch.object(qa, "read_line", side_effect=ValueError("dropped reply")):
                with self.assertRaises(ValueError):
                    qa.command(reopened, process, request, 10**15)
            uncertain = qa.Journal.open(path)
            self.assertEqual(uncertain.value["spent"], 2)
            self.assertIsNotNone(uncertain.value["pending"])
            with self.assertRaises(ValueError):
                uncertain.reserve(1000, 10**15)
            with self.assertRaises(ValueError):
                qa.command(uncertain, process, request, 10**15)

    def test_certain_refusals_keep_the_loop_but_uncertainty_still_stops(self):
        proposal, manifest = inputs()
        release = "sha256:" + "1" * 64
        now = int(qa.time.time() * 1000)
        selection = {**qa.records(proposal, manifest, 1000, now), "release_ids": [release, "sha256:" + "2" * 64]}

        def cli(binary, arguments, value=None):
            if arguments[:2] == ["release", "identity"]:
                return {"verified": True, "release_id": release}
            if arguments[:2] == ["workspace", "qa-runtime"]:
                return {"release_id": release, "conformance": "enforced"}
            raise ValueError("Installed louiselm-control run refused or became unavailable; no automatic retry")

        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            journal = qa.Journal.create(directory / "journal.json")
            output = io.StringIO()
            with patch.object(sys, "stdin", io.StringIO("stauts\nstatus\ncommand\nquit\n")), patch.object(sys, "stdout", output):
                qa.interactive(selection, manifest, journal, directory)
            self.assertIn("Unknown QA verb", output.getvalue())
            self.assertIn("No active QA Session", output.getvalue())
            self.assertEqual(qa.Journal.open(directory / "journal.json").value["next"], 0)

            with patch.object(sys, "stdin", io.StringIO("start\nquit\n")), patch.object(qa, "cli", side_effect=cli), patch.object(sys, "stdout", io.StringIO()):
                with self.assertRaises(ValueError):
                    qa.interactive(selection, manifest, journal, directory)
            retained = qa.Journal.open(directory / "journal.json").value
            self.assertEqual((retained["next"], retained["active"], retained["pending"]), (1, 0, "authorization"))

    def test_lifecycle_receipt_binds_exact_subject_action_and_result(self):
        request = {"session_id": "s", "run_id": "r", "envelope_revision": 1, "request_id": "park-1", "action": "park"}
        receipt = {"signature": "fixture-not-cryptographic-proof", "payload": {
            "session_id": "s", "run_id": "r", "envelope_revision": 1, "request_id": "park-1",
            "outcome": {"action": "park"}, "resulting_state": "parked"}}
        self.assertEqual(qa.receipt_for({"Ok": receipt}, request), receipt)
        for field, value in (("session_id", "other"), ("request_id", "other"), ("resulting_state", "running")):
            changed = copy.deepcopy(receipt)
            changed["payload"][field] = value
            with self.assertRaises(ValueError):
                qa.receipt_for({"Ok": changed}, request)

    def test_explicit_disposal_resolution_reuses_only_retained_disposal_and_requires_settlement(self):
        proposal, manifest = inputs()
        request = qa.records(proposal, manifest, 1000, 1000)["grants"][0]["request"]
        operation = {"schema": "louiselm.launch.lifecycle-request/1", "protocol_version": 1,
                     "request_id": "dispose-1", "session_id": request["session_id"],
                     "run_id": request["run_id"], "authorization_id": request["authorization_id"],
                     "action": "disposal", "expected_state": "running", "expected_receipt_sequence": 3,
                     "envelope_revision": 1}
        receipt = {"signature": "fixture-not-cryptographic-proof", "payload": {
            **operation, "resulting_state": "terminal", "outcome": {"action": "disposal"}}}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            journal = qa.Journal.create(root / "journal.json")
            journal.reserve(1, 2000)
            journal.value["pending"] = operation
            journal.save()
            settled = {"state": "terminal", "broker_head": {"sequence": 4},
                       "launcher_head": {"sequence": 4}, "pending_receipt_count": 0}
            with patch.object(qa, "cli", return_value={"Ok": receipt}) as caller, patch.object(qa, "status_for", return_value={**settled, "pending_receipt_count": 1}):
                with self.assertRaises(ValueError):
                    qa.resolve_disposal(journal, root, request)
                self.assertEqual(caller.call_args.args[2], operation)
                self.assertEqual(qa.Journal.open(journal.path).value["active"], 0)
            with patch.object(qa, "cli", return_value={"Ok": receipt}), patch.object(qa, "status_for", return_value=settled):
                qa.resolve_disposal(journal, root, request)
            self.assertIsNone(qa.Journal.open(journal.path).value["active"])


if __name__ == "__main__":
    unittest.main()
