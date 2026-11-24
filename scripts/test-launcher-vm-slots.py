#!/usr/bin/env python3
"""Offline contracts for credential-free launcher VM slots."""

import json
import importlib.util
from contextlib import redirect_stderr
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import os
import shutil
from types import SimpleNamespace
from unittest.mock import patch


LAUNCHER = Path(__file__).with_name("launcher-vm")
SPEC = importlib.util.spec_from_file_location("slots", LAUNCHER.with_name("launcher-vm-slots.py"))
slots = importlib.util.module_from_spec(SPEC)


class SlotPlan(unittest.TestCase):
    def test_guest_failure_preserves_status_without_printing_command(self):
        error = subprocess.CalledProcessError(42, ["exec", "private-command-payload"])
        output = io.StringIO()
        with patch.object(slots, "main", side_effect=error), redirect_stderr(output):
            with self.assertRaises(SystemExit) as exited:
                slots.cli()
        self.assertEqual(exited.exception.code, 42)
        self.assertIn("exit 42", output.getvalue())
        self.assertNotIn("private-command-payload", output.getvalue())

    def test_private_caches_share_base_but_not_unit_or_port(self):
        plans = []
        with tempfile.TemporaryDirectory(prefix="louiselm-slots-") as directory:
            for name in ("one", "two"):
                result = subprocess.run(
                    ["bash", str(LAUNCHER), "--slot", "plan"],
                    env={**os.environ, "XDG_CACHE_HOME": f"{directory}/{name}"},
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                plans.append(json.loads(result.stdout))
        self.assertEqual(plans[0]["base_image"], plans[1]["base_image"])
        self.assertNotEqual(plans[0]["systemd"], plans[1]["systemd"])
        self.assertNotEqual(plans[0]["ssh_port"], plans[1]["ssh_port"])
        self.assertNotEqual(plans[0]["ssh_port"], 22554)
        self.assertNotEqual(plans[1]["ssh_port"], 22554)

    def test_credential_modes_refused_before_effects(self):
        for args in (["start", "--provider-egress"], ["start", "--yubikey", "003:002"],
                     ["reset", "--discard"], ["terminal"], ["prepare", "--base"]):
            result = subprocess.run(["bash", str(LAUNCHER), "--slot", *args],
                                    capture_output=True, text=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid throwaway slot command", result.stderr)


class PoolContract(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="louiselm-slot-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.pool = slots.Pool(self.directory / "pool", "prepared-0123456789abcdef.qcow2")
        self.source = self.directory / self.pool.base.name
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", str(self.source), "32G"], check=True)
        self.source.chmod(0o400)
        Path(str(self.source) + ".vars.fd").write_bytes(b"firmware")
        Path(str(self.source) + ".vars.fd").chmod(0o400)
        self.owner = patch.object(slots, "verified_owner", return_value=("agent/session-one", "/editor"))
        self.owner.start()
        self.addCleanup(self.owner.stop)
        self.disk = patch.object(slots.shutil, "disk_usage", return_value=shutil_usage(100 * slots.GIB))
        self.disk.start()
        self.addCleanup(self.disk.stop)
        self.runtime = patch.object(self.pool, "runtime")
        self.core = self.runtime.start()
        self.addCleanup(self.runtime.stop)

    def prepare(self, name="one"):
        return self.pool.prepare(self.directory / name, self.source)

    def overlay(self, record):
        overlay = Path(record["state"]) / "run.qcow2"
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "qcow2",
                        "-b", record["base"], str(overlay)], check=True)
        return overlay

    def test_pool_owns_base_after_source_cache_is_deleted(self):
        source_cache = self.directory / "cache-a"
        source_cache.mkdir()
        original = self.source
        self.source = source_cache / original.name
        original.rename(self.source)
        Path(str(original) + ".vars.fd").rename(str(self.source) + ".vars.fd")
        backing = source_cache / "debian.qcow2"
        self.source.rename(backing)
        backing.chmod(0o600)
        subprocess.run(["qemu-io", "-f", "qcow2", "-c", "write -P 0x5a 0 4096", str(backing)],
                       check=True, capture_output=True)
        backing.chmod(0o400)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "qcow2",
                        "-b", str(backing), str(self.source)], check=True)
        self.source.chmod(0o400)
        before = self.source.stat()
        first = self.prepare()
        second = self.prepare("two")
        self.assertEqual(first["base"], second["base"])
        self.assertFalse(self.pool.base.is_symlink())
        self.assertEqual(self.source.stat(), before)
        self.assertEqual(len(list(self.pool.base.parent.glob("*.qcow2"))), 1)
        self.core.assert_not_called()  # no provisioning when a base already exists
        shutil.rmtree(source_cache)
        for record in (first, second):
            overlay = self.overlay(record)
            chain = json.loads(subprocess.run(["qemu-img", "info", "--backing-chain", "--output=json", str(overlay)],
                                             capture_output=True, text=True, check=True).stdout)
            self.assertEqual(chain[0]["backing-filename"], str(self.pool.base))
            self.assertEqual(len(chain), 2)
            self.assertLess(slots.allocated(overlay), slots.GIB // 100)
            subprocess.run(["qemu-io", "-r", "-f", "qcow2", "-c", "read -P 0x5a 0 4096", str(overlay)],
                           check=True, capture_output=True)
        self.assertEqual(Path(str(self.pool.base) + ".vars.fd").read_bytes(), b"firmware")
        self.pool.start(Path(first["state"]).parent)
        self.core.assert_called_once_with(first, "start")

    def test_missing_base_refuses_without_provisioning_or_reserving_slot(self):
        with self.assertRaisesRegex(ValueError, "never provisions or copies another base"):
            self.pool.prepare(self.directory / "one")
        self.core.assert_not_called()
        self.assertEqual(self.pool.records(), [])

    def test_changed_inputs_refuse_start_and_lowered_cap_is_enforced(self):
        record = self.prepare()
        self.prepare("two")
        self.pool.limit = 1
        with self.assertRaisesRegex(ValueError, "slot cap exceeded"):
            self.pool.start(Path(record["state"]).parent)
        self.pool.base = self.pool.base.with_name("prepared-fedcba9876543210.qcow2")
        with self.assertRaisesRegex(ValueError, "older inputs"):
            self.pool.start(Path(record["state"]).parent)
        self.core.assert_not_called()

    def test_prepare_replaces_legacy_links_with_owned_bytes(self):
        self.pool.base.parent.mkdir(mode=0o700, parents=True)
        self.pool.base.symlink_to(self.source)
        Path(str(self.pool.base) + ".vars.fd").symlink_to(str(self.source) + ".vars.fd")
        self.prepare()
        self.assertFalse(self.pool.base.is_symlink())
        self.assertFalse(Path(str(self.pool.base) + ".vars.fd").is_symlink())

    def test_missing_and_dangling_bases_are_reported_and_refuse_start(self):
        record = self.prepare()
        self.pool.base.unlink()
        for expected in ("missing", "dangling"):
            if expected == "dangling":
                self.pool.base.symlink_to(self.directory / "deleted-cache.qcow2")
            with patch.object(slots, "owner_state", return_value="live"), patch.object(
                slots.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout="inactive\n")
            ):
                status = self.pool.status()
            self.assertEqual(status["shared_bases"][0]["state"], expected)
            self.assertEqual(status["slots"][0]["base_state"], expected)
            with self.assertRaisesRegex(ValueError, "shared base.*--slot prepare"):
                self.pool.start(Path(record["state"]).parent)
        self.core.assert_not_called()

    def test_cap_and_floor_include_stopped_slot_growth(self):
        first = self.prepare()
        self.prepare("two")
        with self.assertRaisesRegex(ValueError, "slot cap reached"):
            self.prepare("three")
        with patch.object(slots.shutil, "disk_usage", return_value=shutil_usage(83 * slots.GIB)):
            with self.assertRaisesRegex(ValueError, "84.0 GiB needed"):
                self.pool.start(Path(first["state"]).parent)
        self.core.assert_not_called()

    def test_low_disk_refuses_prepare_without_publishing_or_overlay(self):
        for free in (51, 52):
            with patch.object(slots.shutil, "disk_usage", return_value=shutil_usage(free * slots.GIB)):
                with self.assertRaisesRegex(ValueError, "52.0 GiB needed"):
                    self.prepare()
        self.assertFalse(self.pool.base.exists())
        self.assertEqual(self.pool.records(), [])

    def test_live_foreign_owner_cannot_connect_stop_or_take_slot(self):
        record = self.prepare()
        with patch.object(slots, "verified_owner", return_value=("agent/session-two", "/editor")):
            for operation in (lambda: self.pool.prepare(Path(record["state"]).parent),
                              lambda: self.pool.start(Path(record["state"]).parent),
                              lambda: self.pool.cleanup(Path(record["state"]).parent),
                              lambda: self.pool.owned(record)):
                with self.assertRaisesRegex(ValueError, "slot owned by agent/session-one"):
                    operation()
        self.core.assert_not_called()

    def test_cleanup_gone_preserves_live_unknown_bases_and_logs(self):
        first = self.prepare()
        second = self.prepare("two")
        first_overlay = self.overlay(first)
        second_overlay = self.overlay(second)
        log = Path(first["state"]) / "serial.log"
        log.write_text("retained diagnostic\n")
        with patch.object(slots, "owner_state", return_value="unknown"):
            self.pool.cleanup(self.directory / "unused", gone=True)
        self.assertTrue(first_overlay.exists())
        self.core.assert_not_called()
        with patch.object(slots, "owner_state", side_effect=lambda r: "gone" if r == first else "live"):
            self.pool.cleanup(self.directory / "unused", gone=True)
        self.core.assert_called_once_with(first, "stop")
        self.assertFalse(first_overlay.exists())
        self.assertTrue(second_overlay.exists())
        self.assertTrue(self.pool.base.exists())
        self.assertTrue(self.source.exists())
        self.assertEqual(log.read_text(), "retained diagnostic\n")
        self.assertEqual(self.pool.records(), [second])

    def test_owner_death_is_rechecked_before_cleanup(self):
        record = self.prepare()
        overlay = self.overlay(record)
        with patch.object(slots, "owner_state", side_effect=["gone", "live"]):
            self.pool.cleanup(self.directory / "unused", gone=True)
        self.assertTrue(overlay.exists())
        self.core.assert_not_called()

    def test_cleanup_failure_keeps_overlay_and_reservation(self):
        record = self.prepare()
        overlay = self.overlay(record)
        self.core.side_effect = subprocess.CalledProcessError(1, ["stop"])
        with self.assertRaises(subprocess.CalledProcessError):
            self.pool.cleanup(Path(record["state"]).parent)
        self.assertTrue(overlay.exists())
        self.assertEqual(self.pool.records(), [record])

    def test_retained_or_default_directories_are_never_adopted(self):
        state = self.directory / "retained" / "louiselm-launcher-vm"
        state.mkdir(mode=0o700, parents=True)
        disk = state / "run.qcow2"
        disk.write_bytes(b"protected evidence")
        with self.assertRaisesRegex(ValueError, "not a throwaway slot"):
            self.pool.prepare(state.parent, self.source)
        self.assertEqual(disk.read_bytes(), b"protected evidence")
        self.core.assert_not_called()

    def test_readonly_chain_required_and_no_chmod_of_source(self):
        self.source.chmod(0o600)
        with self.assertRaisesRegex(ValueError, "already be owned and read-only"):
            self.prepare()
        self.assertEqual(self.source.stat().st_mode & 0o777, 0o600)

    def test_admission_and_slot_commands_exclude_each_other(self):
        record = self.prepare()
        with slots.locked(self.pool.root / "admission.lock"):
            with self.assertRaisesRegex(ValueError, "another slot command"):
                self.prepare("two")
        with slots.locked(Path(record["state"]) / "slot.lock"):
            with self.assertRaisesRegex(ValueError, "another slot command"):
                self.pool.cleanup(Path(record["state"]).parent)
        self.core.assert_not_called()

    def test_port_collision_refuses_and_status_reports_owner_age_disk(self):
        first = self.prepare()
        original = self.pool.layout
        with patch.object(self.pool, "layout", side_effect=lambda p: {**original(p), "port": first["port"]}):
            with self.assertRaisesRegex(ValueError, "port collision"):
                self.prepare("two")
        self.overlay(first)
        with patch.object(slots, "owner_state", return_value="live"), patch.object(
            slots.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout="active\n")
        ):
            status = self.pool.status()
        row = status["slots"][0]
        self.assertEqual(row["owner"], "agent/session-one")
        self.assertEqual(row["owner_state"], "live")
        self.assertGreaterEqual(row["age_seconds"], 0)
        self.assertGreater(row["disk_bytes"], 0)
        self.assertEqual(len(status["shared_bases"]), 1)

    def test_liveness_uses_exact_editor_identity_and_protects_unknown(self):
        record = self.prepare()
        for answer, expected in ((record["owner"], "live"),
                                 ("no live Session has ACP session id session-one", "gone"),
                                 ("ambiguous Session", "unknown")):
            with patch.object(slots, "identity", return_value=answer):
                self.assertEqual(slots.owner_state(record), expected)
        with patch.object(slots, "identity", return_value=""), patch.object(
            slots.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout="")
        ):
            self.assertEqual(slots.owner_state(record), "unknown")

    def test_unreachable_editor_uses_transcript_expiry_and_timeout_is_unknown(self):
        record = self.prepare()
        for expiry, expected in (("2000-01-01T00:00:00Z", "gone"),
                                 ("2999-01-01T00:00:00Z", "live")):
            evidence = json.dumps({"holder": record["owner"], "expires_ts": expiry})
            with patch.object(slots, "identity", return_value=""), patch.object(
                slots.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout=evidence)
            ):
                self.assertEqual(slots.owner_state(record), expected)
        with patch.object(slots.subprocess, "run", side_effect=subprocess.TimeoutExpired("nvim", 10)):
            self.assertEqual(slots.identity("/editor", "session-one"), "")

    def test_owner_must_be_verified_not_just_supplied(self):
        # Call the real function, bypassing only this fixture's default patch.
        with patch.dict(os.environ, {"LOUISELM_VM_OWNER": "agent/session-one", "NVIM": "/editor"}), \
                patch.object(slots, "identity", return_value="agent/other-session"):
            with self.assertRaisesRegex(ValueError, "did not verify"):
                REAL_VERIFIED_OWNER()


def shutil_usage(free):
    return SimpleNamespace(free=free)


if __name__ == "__main__":
    sys.dont_write_bytecode = True
    SPEC.loader.exec_module(slots)
    REAL_VERIFIED_OWNER = slots.verified_owner
    os.umask(0o077)
    unittest.main()
