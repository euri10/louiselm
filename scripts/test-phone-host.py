#!/usr/bin/env python3
"""Functional tests for the phone-host provisioning candidate."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("phone_host", Path(__file__).with_name("phone-host.py"))
HOST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HOST)


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

    def test_missing_records_do_not_create_anything(self):
        self.assertEqual(HOST.inspect(self.paths, self.root, self.receiver), "catalog_missing")
        self.assertFalse((self.base / "var").exists())

    def test_install_restart_and_alias_preserve_exact_catalog(self):
        HOST.install(self.paths, self.root, self.receiver)
        catalog = self.base / HOST.CATALOG
        original = catalog.read_bytes()
        HOST.install(self.paths, self.alias, self.receiver)
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
        HOST.install(self.paths, self.root, self.receiver)
        HOST.refresh(self.paths, require_pid_one=False)
        catalog = self.base / HOST.CATALOG
        reference = self.base / HOST.REFERENCE
        prior = (catalog.read_bytes(), reference.read_bytes())
        other = self.base / "other"
        other.mkdir()
        for root, receiver in ((other, self.receiver), (self.root, "b" * 64)):
            with self.assertRaisesRegex(ValueError, "existing record differs"):
                HOST.install(self.paths, root, receiver)
        self.assertEqual((catalog.read_bytes(), reference.read_bytes()), prior)
        self.assertEqual(HOST.inspect(self.paths, other, self.receiver), "root_mismatch")
        self.assertEqual(HOST.inspect(self.paths, self.root, "b" * 64), "receiver_mismatch")

    def test_missing_or_changed_reference_is_unavailable(self):
        HOST.install(self.paths, self.root, self.receiver)
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
        HOST.install(self.paths, self.root, self.receiver)
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


if __name__ == "__main__":
    unittest.main()
