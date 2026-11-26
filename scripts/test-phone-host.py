#!/usr/bin/env python3
"""Functional tests for the phone-host provisioning candidate."""

import importlib.util
import errno
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("phone_host", Path(__file__).with_name("phone-host.py"))
HOST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HOST)
SAFE_EXECUTABLE = HOST.safe_executable
FILESYSTEM_DEVICE = HOST.filesystem_device
QUERY_TEMPLATES = HOST.query_templates


class PhoneHostTest(unittest.TestCase):
    def setUp(self):
        self.tmp = self.enterContext(tempfile.TemporaryDirectory())
        self.base = Path(self.tmp)
        self.base.chmod(0o755)
        self.root = self.base / "capture"
        self.root.mkdir()
        self.alias = self.base / "alias"
        self.alias.symlink_to(self.root, target_is_directory=True)
        self.receiver = "a" * 64
        self.paths = HOST.Paths(self.base, os.geteuid())
        self.handles = {}

        def handle(fd):
            info = os.fstat(fd)
            value = f"{info.st_dev:016x}{info.st_ino:016x}"
            if value not in self.handles:
                self.handles[value] = os.dup(fd)
                self.addCleanup(os.close, self.handles[value])
            return 1, value

        def reopen(record):
            try:
                return os.dup(self.handles[record["handle"]])
            except KeyError:
                raise OSError(errno.ESTALE, "stale handle") from None

        self.enterContext(mock.patch.object(HOST, "filesystem_uuid", create=True,
                                           return_value="11111111-1111-4111-8111-111111111111"))
        self.enterContext(mock.patch.object(HOST, "uuid_device", create=True,
                                           return_value=self.root.stat().st_dev))
        self.enterContext(mock.patch.object(HOST, "file_handle", create=True, side_effect=handle))
        self.enterContext(mock.patch.object(HOST, "reopen_handle", create=True, side_effect=reopen))
        self.attach = self.enterContext(mock.patch.object(HOST, "attach_root", create=True))
        self.enterContext(mock.patch.object(HOST, "safe_executable", create=True,
                                           return_value="/opt/louiselm/bin/capture"))
        self.enterContext(mock.patch.object(HOST, "query_templates", create=True, return_value={
            "louiselm-phone-pairing.socket": b"User=@OPERATOR_UID@\nGroup=@OPERATOR_GID@\n",
            "louiselm-phone-pairing@.service": b"User=@OPERATOR_UID@\nGroup=@OPERATOR_GID@\n"
            b"ExecStart=@CAPTURE_BINARY@\nOpenFile=@CAPTURE_ROOT@\n",
        }))

    def install(self, root, receiver):
        HOST.install(self.paths, root, receiver, 1001, 1001, "/opt/louiselm/bin/capture")

    def test_missing_records_do_not_create_anything(self):
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "catalog_missing")
        self.assertFalse((self.base / "var").exists())

    def test_install_restart_and_alias_preserve_exact_catalog(self):
        self.install(self.root, self.receiver)
        catalog = self.base / HOST.CATALOG
        original = catalog.read_bytes()
        self.install(self.alias, self.receiver)
        self.assertEqual(catalog.read_bytes(), original)
        HOST.refresh(self.paths, require_pid_one=False)
        reference = self.base / HOST.REFERENCE
        prior = reference.read_bytes()
        HOST.refresh(self.paths, require_pid_one=False)
        self.assertEqual(reference.read_bytes(), prior)
        self.assertEqual(HOST.inspect(self.paths, self.alias, self.receiver), "scope_unverified")
        self.assertEqual(catalog.stat().st_mode & 0o777, 0o444)
        self.assertEqual(reference.stat().st_mode & 0o777, 0o444)

    def test_conflicting_registration_preserves_existing_records(self):
        self.install(self.root, self.receiver)
        HOST.refresh(self.paths, require_pid_one=False)
        catalog = self.base / HOST.CATALOG
        reference = self.base / HOST.REFERENCE
        prior = (catalog.read_bytes(), reference.read_bytes())
        other = self.base / "other"
        other.mkdir()
        for root, receiver in ((other, self.receiver), (self.root, "b" * 64)):
            with self.assertRaisesRegex(ValueError, "existing record differs"):
                self.install(root, receiver)
        self.assertEqual((catalog.read_bytes(), reference.read_bytes()), prior)
        self.assertEqual(HOST.inspect(self.paths, other, self.receiver), "root_mismatch")
        self.assertEqual(HOST.inspect(self.paths, self.root, "b" * 64), "receiver_mismatch")
        self.assertEqual(HOST.inspect(self.paths, self.base / "missing", self.receiver), "root_unavailable")

    def test_missing_or_changed_reference_is_unavailable(self):
        self.install(self.root, self.receiver)
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "reference_missing")
        HOST.refresh(self.paths, require_pid_one=False)
        reference = self.base / HOST.REFERENCE
        reference.chmod(0o644)
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "reference_invalid")
        reference.chmod(0o444)
        data = json.loads(reference.read_bytes())
        data["boot_id"] = "00000000-0000-4000-8000-000000000000"
        reference.chmod(0o644)
        reference.write_text(json.dumps(data) + "\n")
        reference.chmod(0o444)
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "boot_mismatch")
        with self.assertRaisesRegex(ValueError, "existing record differs"):
            HOST.refresh(self.paths, require_pid_one=False)

    def test_malformed_catalog_and_symlink_are_refused(self):
        self.install(self.root, self.receiver)
        catalog = self.base / HOST.CATALOG
        catalog.chmod(0o644)
        catalog.write_text('{"version":1,"extra":true}\n')
        catalog.chmod(0o444)
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "catalog_invalid")
        with self.assertRaisesRegex(ValueError, "catalog is invalid"):
            HOST.refresh(self.paths, require_pid_one=False)
        catalog.unlink()
        catalog.symlink_to(self.base / "elsewhere")
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "catalog_invalid")

    def test_registration_records_a_reopenable_filesystem_identity(self):
        self.install(self.root, self.receiver)
        value = json.loads((self.base / HOST.CATALOG).read_bytes())
        self.assertEqual(value["version"], 2)
        self.assertIn("filesystem_uuid", value)
        self.assertIn("handle_type", value)
        self.assertTrue(value["handle"])
        self.assertNotIn("inode", value)

    def test_refresh_does_not_publish_when_registered_root_cannot_reopen(self):
        self.install(self.root, self.receiver)
        with mock.patch.object(HOST, "open_registered_root", create=True,
                               side_effect=OSError(errno.ESTALE, "stale handle")):
            with self.assertRaisesRegex(OSError, "stale handle"):
                HOST.refresh(self.paths, require_pid_one=False)
        self.assertFalse((self.base / HOST.REFERENCE).exists())
        self.attach.assert_not_called()

    def test_refresh_reopens_and_holds_the_registered_directory(self):
        (self.root / "marker").write_text("registered", encoding="utf-8")
        self.install(self.root, self.receiver)
        moved = self.base / "moved"
        self.root.rename(moved)

        def check_root(paths, fd):
            with os.fdopen(os.open("marker", os.O_RDONLY, dir_fd=fd), "r") as marker:
                self.assertEqual(marker.read(), "registered")

        self.attach.side_effect = check_root
        HOST.refresh(self.paths, require_pid_one=False)
        self.attach.assert_called_once()

    def test_install_refuses_unsupported_handles_before_publishing(self):
        with mock.patch.object(HOST, "file_handle", side_effect=OSError(errno.EOPNOTSUPP, "unsupported")):
            with self.assertRaisesRegex(OSError, "unsupported"):
                self.install(self.root, self.receiver)
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_install_refuses_a_handle_that_reopens_another_directory(self):
        other = self.base / "other"
        other.mkdir()
        with mock.patch.object(HOST, "reopen_handle", side_effect=lambda _: os.open(other, os.O_RDONLY)):
            with self.assertRaisesRegex(ValueError, "reopened a different directory"):
                self.install(self.root, self.receiver)
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_install_publishes_the_unprivileged_query_units(self):
        self.install(self.root, self.receiver)
        unit = (self.base / "etc/systemd/system/louiselm-phone-pairing@.service").read_text()
        socket = (self.base / "etc/systemd/system/louiselm-phone-pairing.socket").read_text()
        self.assertIn("User=1001\nGroup=1001\n", unit)
        self.assertIn("ExecStart=/opt/louiselm/bin/capture\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/root\n", unit)
        self.assertIn("User=1001\nGroup=1001\n", socket)
        self.assertNotIn("@", socket)

    def test_shipped_query_unit_consumes_the_held_root(self):
        with mock.patch.object(HOST, "query_templates", side_effect=QUERY_TEMPLATES):
            self.install(self.root, self.receiver)
        unit = (self.base / "etc/systemd/system/louiselm-phone-pairing@.service").read_text()
        self.assertIn("Requires=louiselm-phone-host.service\n", unit)
        self.assertIn("After=louiselm-phone-host.service\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/root:root:read-only\n", unit)
        self.assertIn("ExecStart=/opt/louiselm/bin/capture managed-pairing-status\n", unit)

    def test_unsafe_executable_parent_is_rejected(self):
        binary = self.base / "capture-bin"
        binary.write_text("fixture", encoding="utf-8")
        binary.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "untrusted owner or writable component"):
            SAFE_EXECUTABLE(binary, os.geteuid(), os.getegid())

    def test_missing_query_placeholder_does_not_publish_catalog(self):
        templates = {
            "louiselm-phone-pairing.socket": b"User=@OPERATOR_UID@ Group=@OPERATOR_GID@\n",
            "louiselm-phone-pairing@.service": b"ExecStart=@CAPTURE_BINARY@\n",
        }
        with mock.patch.object(HOST, "query_templates", return_value=templates):
            with self.assertRaisesRegex(ValueError, "missing a required placeholder"):
                self.install(self.root, self.receiver)
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_duplicate_mounted_filesystem_uuid_is_unavailable(self):
        uuid = "11111111-1111-4111-8111-111111111111"
        with mock.patch.object(HOST, "uuid_device", return_value=123), \
                mock.patch.object(HOST, "mount_entries", return_value=[(456, b"/other", b"ext4")]), \
                mock.patch.object(HOST, "probe_device", return_value=uuid):
            with self.assertRaisesRegex(ValueError, "UUID is ambiguous"):
                FILESYSTEM_DEVICE(uuid)

    def test_conflicting_live_mount_does_not_publish_reference(self):
        self.install(self.root, self.receiver)
        self.attach.side_effect = ValueError("existing root mount differs")
        with self.assertRaisesRegex(ValueError, "existing root mount differs"):
            HOST.refresh(self.paths, require_pid_one=False)
        self.assertFalse((self.base / HOST.REFERENCE).exists())


if __name__ == "__main__":
    unittest.main()
