#!/usr/bin/env python3
"""Unprivileged regressions for Admission setup and shared bounded diagnostics."""

from contextlib import nullcontext
import os
from pathlib import Path
import runpy
import shutil
import tempfile
import unittest
from unittest import mock


FIXTURE = runpy.run_path(str(Path(__file__).with_name("test-broker-admission.py")))
DIAGNOSTICS = runpy.run_path(str(Path(__file__).with_name("test-broker-attention-diagnostics.py")))
INSTALLER = runpy.run_path(str(Path(__file__).with_name("install-broker-admission.py")))


class AdmissionDiagnostics(DIAGNOSTICS["Diagnostics"]):
    component = "broker-admission"


class AdmissionSetup(unittest.TestCase):
    def test_setup_enters_component_diagnostics(self):
        case = FIXTURE["LinkedAdmission"]("test_cli_outage_restart_and_read_only_evidence")
        diagnostics = mock.MagicMock()
        with mock.patch.dict(FIXTURE["COMMON"], {"diagnostics": diagnostics}):
            try:
                case.setUp()
                diagnostics.assert_called_once_with(component="broker-admission")
                self.assertIs(case.phase, diagnostics.return_value.__enter__.return_value)
            finally:
                case.doCleanups()
        diagnostics.return_value.__exit__.assert_called_once()

    def test_actual_setup_uses_shared_private_mount_without_copying_etc(self):
        case = FIXTURE["LinkedAdmission"]("test_cli_outage_restart_and_read_only_evidence")

        class MountBoundary(Exception):
            pass

        mount = mock.Mock(side_effect=MountBoundary)
        diagnostics = mock.MagicMock()
        forbidden = mock.Mock(side_effect=AssertionError("privileged commands must not run"))
        with tempfile.TemporaryDirectory(prefix="admission-diagnostics-") as temporary:
            with (mock.patch.dict(FIXTURE["COMMON"], {"mount_private_etc": mount,
                                                       "diagnostics": diagnostics}),
                  mock.patch.dict(case.test_cli_outage_restart_and_read_only_evidence.__globals__,
                                  {"command": forbidden}),
                  mock.patch.dict(os.environ, {"LOUISELM_TEST_ADMISSION_BROKER": "/fixture/broker",
                                               "LOUISELM_TEST_SKILLS": "/fixture/skills"}),
                  mock.patch.object(os, "geteuid", return_value=0),
                  mock.patch.object(os, "readlink", side_effect=["private-mount", "host-mount"]),
                  mock.patch.object(runpy, "run_path", return_value={}),
                  mock.patch.object(tempfile, "TemporaryDirectory", return_value=nullcontext(temporary)),
                  mock.patch.object(shutil, "copytree", side_effect=AssertionError("Admission must not copy /etc"))):
                try:
                    case.setUp()
                    with self.assertRaises(MountBoundary):
                        case.test_cli_outage_restart_and_read_only_evidence()
                finally:
                    case.doCleanups()
            mount.assert_called_once_with(Path(temporary))
            diagnostics.return_value.__enter__.return_value.assert_any_call("mount private /etc")
            forbidden.assert_not_called()


class AdmissionEvidencePlan(unittest.TestCase):
    def setUp(self):
        # The production plan rejects world-writable ancestors such as /tmp.
        temporary = self.enterContext(tempfile.TemporaryDirectory(
            prefix=".admission-plan-", dir=Path.home()))
        self.store = Path(temporary)
        for directory in ("packages", "staging", "trust", "generations", "endorsements", "endorsers"):
            (self.store / directory).mkdir()
        for name in ("provenance.json", "trust/roles.json", "trust/roles.lock", "endorsers/local.json",
                     "endorsers/local.lock", "endorsements/sha256-record.json", "quarantine.json", "private-key"):
            (self.store / name).write_text("fixture")

    def test_withdrawal_evidence_is_shared_but_private_lock_and_keys_are_not(self):
        targets = dict(INSTALLER["plan"](self.store, os.geteuid()))
        for name in ("endorsements", "endorsers"):
            self.assertTrue(targets.get(self.store / name), f"{name} must be shared read-only")
        for name in ("endorsements/sha256-record.json", "endorsers/local.json", "quarantine.json"):
            self.assertIn(self.store / name, targets)
            self.assertFalse(targets[self.store / name])
        self.assertNotIn(self.store / "endorsers/local.lock", targets)
        self.assertNotIn(self.store / "private-key", targets)

    def test_withdrawal_evidence_refuses_symlinks(self):
        record = self.store / "endorsements/sha256-record.json"
        record.unlink()
        record.symlink_to(self.store / "private-key")
        with self.assertRaisesRegex(ValueError, "unexpected evidence inode"):
            INSTALLER["plan"](self.store, os.geteuid())


if __name__ == "__main__":
    unittest.main()
