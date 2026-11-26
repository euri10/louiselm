#!/usr/bin/env python3
"""Functional tests for the phone-host provisioning candidate."""

import importlib.util
import errno
import json
import multiprocessing
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
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
        self.writer = self.base / "trusted-phone-writer"
        self.writer.write_bytes(b"fixture writer")
        self.writer.chmod(0o755)
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
        self.enterContext(mock.patch.object(HOST, "host_mounts", create=True,
                                           return_value=[(123, self.root.stat().st_dev,
                                                          os.fsencode(self.base), b"ext4")]))
        self.enterContext(mock.patch.object(HOST, "mount_id", create=True, return_value=123))
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

    def install_managed(self, root, receiver):
        HOST.install(self.paths, root, receiver, 1001, 1001, "/opt/louiselm/bin/capture",
                     pin_owner_parent=True, writer_binary=self.writer)

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

    def test_private_record_publication_is_idempotent_across_processes(self):
        context = multiprocessing.get_context("fork")
        start = context.Barrier(3)
        results = context.Queue()
        record = Path("private/registration.json")
        content = b'{"version":1}\n'

        def publish():
            try:
                start.wait(timeout=10)
                HOST.publish(self.paths, record, content, 0o600)
                results.put(None)
            except Exception as error:
                results.put(repr(error))

        processes = [context.Process(target=publish) for _ in range(2)]
        for process in processes:
            process.start()
        try:
            start.wait(timeout=10)
            self.assertEqual([results.get(timeout=10) for _ in processes], [None, None])
            for process in processes:
                process.join(timeout=10)
                self.assertEqual(process.exitcode, 0)
            path = self.base / record
            self.assertEqual(path.read_bytes(), content)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(sorted(item.name for item in path.parent.iterdir()), [path.name])
            HOST.publish(self.paths, record, content, 0o600)
            with self.assertRaisesRegex(ValueError, "existing record differs"):
                HOST.publish(self.paths, record, b"different\n", 0o600)
            self.assertEqual(path.read_bytes(), content)
        finally:
            for process in processes:
                if process.is_alive():
                    process.terminate()
                process.join()
            results.close()

    def test_writer_installation_preserves_exact_executable_bytes(self):
        source = b"fixture executable\n" * 5000
        path = HOST.PROGRAM.parent / "phone-writer"
        HOST.publish(self.paths, path, source, 0o555, max_bytes=len(source))
        HOST.publish(self.paths, path, source, 0o555, max_bytes=len(source))
        installed = self.base / path
        self.assertEqual(installed.read_bytes(), source)
        self.assertEqual(installed.stat().st_mode & 0o777, 0o555)
        with self.assertRaisesRegex(ValueError, "existing record differs"):
            HOST.publish(self.paths, path, b"replacement", 0o555, max_bytes=len(source))
        self.assertEqual(installed.read_bytes(), source)
        with self.assertRaisesRegex(ValueError, "oversized"):
            HOST.publish(self.paths, path, source, 0o555, max_bytes=len(source) - 1)

    def test_identical_publication_during_first_publisher_completion(self):
        context = multiprocessing.get_context("fork")
        published, release = context.Event(), context.Event()
        results = context.Queue()
        path = Path("private/concurrent.json")
        content = b"same record\n"

        def first():
            real_link = os.link
            real_call = HOST.libc_call

            def hold_link(*args, **kwargs):
                result = real_link(*args, **kwargs)
                published.set()
                if not release.wait(10):
                    raise TimeoutError("second publisher did not finish")
                return result

            def hold_rename(name, arguments, argtypes):
                result = real_call(name, arguments, argtypes)
                if name == "renameat2":
                    published.set()
                    if not release.wait(10):
                        raise TimeoutError("second publisher did not finish")
                return result

            try:
                with mock.patch.object(HOST.os, "link", side_effect=hold_link), \
                        mock.patch.object(HOST, "libc_call", side_effect=hold_rename):
                    HOST.publish(self.paths, path, content, 0o600)
                results.put(None)
            except Exception as error:
                results.put(repr(error))

        process = context.Process(target=first)
        process.start()
        first_error = None
        try:
            self.assertTrue(published.wait(10), "first publisher did not publish")
            links_during_publication = (self.base / path).stat().st_nlink
            try:
                HOST.publish(self.paths, path, content, 0o600)
            except Exception as error:
                self.fail(f"identical publication failed with {error!r} while link count was {links_during_publication}")
            self.assertEqual(links_during_publication, 1)
        finally:
            release.set()
            process.join(timeout=10)
            if process.is_alive():
                process.terminate()
                process.join()
            if process.exitcode == 0:
                first_error = results.get(timeout=2)
            results.close()
        self.assertEqual(process.exitcode, 0)
        self.assertIsNone(first_error)
        self.assertEqual((self.base / path).read_bytes(), content)

    def test_publication_refuses_when_atomic_no_replace_is_unsupported(self):
        path = Path("private/unsupported.json")
        with mock.patch.object(HOST, "libc_call", side_effect=OSError(errno.EOPNOTSUPP, "unsupported")):
            with self.assertRaisesRegex(OSError, "unsupported"):
                HOST.publish(self.paths, path, b"record\n", 0o600)
        self.assertFalse((self.base / path).exists())
        self.assertEqual(list((self.base / path.parent).iterdir()), [])

    def test_installer_copies_only_a_trusted_writer_to_the_fixed_location(self):
        build = self.base / "trusted-build"
        build.mkdir(mode=0o755)
        source = build / "phone-writer"
        data = b"fixture executable\n" * 5000
        source.write_bytes(data)
        source.chmod(0o755)
        HOST.install_writer(self.paths, source)
        HOST.install_writer(self.paths, source)
        installed = self.base / HOST.WRITER
        self.assertEqual(installed.read_bytes(), data)
        self.assertEqual(installed.stat().st_mode & 0o777, 0o555)
        source.write_bytes(b"different writer")
        with self.assertRaisesRegex(ValueError, "existing record differs"):
            HOST.install_writer(self.paths, source)
        self.assertEqual(installed.read_bytes(), data)

    def test_installer_refuses_a_writable_writer_source(self):
        source = self.base / "phone-writer"
        source.write_bytes(b"fixture executable")
        source.chmod(0o775)
        with self.assertRaisesRegex(ValueError, "untrusted writer"):
            HOST.install_writer(self.paths, source)
        self.assertFalse((self.base / HOST.WRITER).exists())

    def test_private_staging_uses_a_trusted_mount_on_the_destination_filesystem(self):
        device = self.root.stat().st_dev
        mounts = [(123, device, os.fsencode(self.base), b"ext4")]
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, root_fd)
        with mock.patch.object(HOST, "host_mounts", return_value=mounts), \
                mock.patch.object(HOST, "mount_id", return_value=123):
            first = HOST.open_filesystem_stage(self.paths, root_fd)
            second = HOST.open_filesystem_stage(self.paths, root_fd)
        try:
            self.assertTrue(HOST.same_directory(first, second))
            info = os.fstat(first)
            self.assertEqual(info.st_dev, device)
            self.assertEqual(info.st_uid, self.paths.owner)
            self.assertEqual(info.st_mode & 0o777, 0o700)
        finally:
            os.close(first)
            os.close(second)

    def test_private_staging_refuses_an_untrusted_mount_before_creating_a_directory(self):
        device = self.root.stat().st_dev
        mounts = [(123, device, os.fsencode(self.base), b"ext4")]
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, root_fd)
        self.base.chmod(0o775)
        with mock.patch.object(HOST, "host_mounts", return_value=mounts), \
                mock.patch.object(HOST, "mount_id", return_value=123):
            with self.assertRaisesRegex(ValueError, "trusted.*mount"):
                HOST.open_filesystem_stage(self.paths, root_fd)
        self.assertFalse((self.base / HOST.STAGE_NAME).exists())

    def test_private_staging_rejects_a_mount_with_a_different_kernel_identity(self):
        device = self.root.stat().st_dev
        mounts = [(123, device, os.fsencode(self.base), b"ext4")]
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, root_fd)
        with mock.patch.object(HOST, "host_mounts", return_value=mounts), \
                mock.patch.object(HOST, "mount_id", return_value=124):
            with self.assertRaisesRegex(ValueError, "trusted.*mount"):
                HOST.open_filesystem_stage(self.paths, root_fd)
        self.assertFalse((self.base / HOST.STAGE_NAME).exists())

    def test_private_staging_preserves_and_refuses_an_existing_unsafe_directory(self):
        device = self.root.stat().st_dev
        mounts = [(123, device, os.fsencode(self.base), b"ext4")]
        stage = self.base / HOST.STAGE_NAME
        stage.mkdir(mode=0o755)
        (stage / "keep").write_text("prior state", encoding="utf-8")
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, root_fd)
        with mock.patch.object(HOST, "host_mounts", return_value=mounts), \
                mock.patch.object(HOST, "mount_id", return_value=123):
            with self.assertRaisesRegex(ValueError, "staging directory is untrusted"):
                HOST.open_filesystem_stage(self.paths, root_fd)
        self.assertEqual((stage / "keep").read_text(encoding="utf-8"), "prior state")
        self.assertEqual(stage.stat().st_mode & 0o777, 0o755)

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

    def test_managed_setup_pins_both_capture_and_its_original_parent(self):
        self.install_managed(self.alias, self.receiver)
        original = (self.base / HOST.CATALOG).read_bytes()
        record = json.loads(original)
        self.assertEqual(record["version"], 3)
        self.assertEqual((record["operator_uid"], record["operator_gid"]), (1001, 1001))
        self.assertEqual(record["capture_stage"], record["parent_stage"])
        capture = HOST.open_registered_root(self.paths)
        parent = HOST.open_registered_parent(self.paths)
        try:
            self.assertEqual(os.fstat(capture).st_ino, self.root.stat().st_ino)
            self.assertEqual(os.fstat(parent).st_ino, self.base.stat().st_ino)
            self.assertNotEqual(os.fstat(capture).st_ino, os.fstat(parent).st_ino)
        finally:
            os.close(capture)
            os.close(parent)
        self.install_managed(self.root, self.receiver)
        self.assertEqual((self.base / HOST.CATALOG).read_bytes(), original)
        HOST.refresh(self.paths, require_pid_one=False)
        self.assertEqual(self.attach.call_count, 4)
        self.assertEqual(self.attach.call_args_list[1].args[2], HOST.HELD_OWNER_PARENT)
        self.assertEqual(self.attach.call_args_list[2].args[2], HOST.HELD_CAPTURE_STAGE)
        self.assertEqual(self.attach.call_args_list[3].args[2], HOST.HELD_PARENT_STAGE)

    def test_managed_setup_refuses_a_directory_without_the_expected_parent_binding(self):
        other = self.base / "not-capture"
        other.mkdir()
        with self.assertRaisesRegex(ValueError, "capture parent"):
            self.install_managed(other, self.receiver)
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_managed_repeat_reopens_the_registered_stage_instead_of_selecting_a_new_mount(self):
        self.install_managed(self.root, self.receiver)
        original = (self.base / HOST.CATALOG).read_bytes()
        with mock.patch.object(HOST, "open_filesystem_stage", side_effect=AssertionError("new stage selected")):
            self.install_managed(self.alias, self.receiver)
        self.assertEqual((self.base / HOST.CATALOG).read_bytes(), original)

    def test_managed_installer_supplies_both_held_descriptors_to_the_fixed_writer(self):
        self.install_managed(self.root, self.receiver)
        unit = (self.base / "etc/systemd/system/louiselm-phone-registration@.service").read_text()
        socket = (self.base / "etc/systemd/system/louiselm-phone-registration.socket").read_text()
        self.assertIn("ExecStart=/usr/local/lib/louiselm-phone-host/phone-writer serve\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/root:root:read-only\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/owner-parent:parent:read-only\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/capture-stage:capture_stage:read-only\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/parent-stage:parent_stage:read-only\n", unit)
        self.assertIn("StateDirectory=louiselm-phone-authority\n", unit)
        self.assertIn("StateDirectoryMode=0700\n", unit)
        self.assertIn("OpenFile=/var/lib/louiselm-phone-authority:authority:read-only\n", unit)
        self.assertIn("OpenFile=/var/lib/louiselm-phone-host/catalog.json:catalog:read-only\n", unit)
        self.assertIn("OpenFile=/run/louiselm-phone-host/reference.json:reference:read-only\n", unit)
        self.assertEqual((self.base / HOST.PRIVATE_STATE).stat().st_mode & 0o777, 0o700)
        self.assertIn("SocketUser=1001\nSocketGroup=1001\n", socket)
        self.assertIn("ListenStream=/run/louiselm-phone-host/registration.sock\n", socket)
        self.assertEqual((self.base / HOST.WRITER).read_bytes(), self.writer.read_bytes())
        self.assertEqual((self.base / HOST.WRITER).stat().st_mode & 0o777, 0o555)

    def test_managed_setup_does_not_upgrade_an_existing_catalog(self):
        self.install(self.root, self.receiver)
        catalog = self.base / HOST.CATALOG
        original = catalog.read_bytes()
        with self.assertRaisesRegex(ValueError, "existing.*recovery"):
            self.install_managed(self.root, self.receiver)
        self.assertEqual(catalog.read_bytes(), original)
        self.assertFalse((self.base / HOST.WRITER).exists())
        self.assertFalse((self.base / HOST.STAGE_NAME).exists())
        self.assertFalse((self.base / "etc/systemd/system/louiselm-phone-registration.socket").exists())

    def test_first_install_runs_the_fixed_writer_after_private_setup(self):
        calls = []

        def prepare(argv, **kwargs):
            calls.append(argv)
            self.assertEqual(argv[0], str(self.base / HOST.WRITER))
            self.assertEqual(argv[1], "prepare")
            private_fd = int(argv[2])
            self.assertIn(private_fd, kwargs["pass_fds"])
            setup = HOST.decode(HOST.read(self.paths, HOST.PRIVATE_STATE / "setup.json", 0o600), {
                "version": int, "filesystem_uuid": str, "handle_type": int, "handle": str,
                "owner_parent": dict, "capture_stage": dict, "parent_stage": dict,
                "operator_uid": int, "operator_gid": int,
            })
            self.assertEqual(setup["version"], 3)
            for name in ("candidate.intent", "candidate.prepared"):
                try:
                    fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=private_fd)
                except FileExistsError:
                    continue
                with os.fdopen(fd, "wb") as marker:
                    marker.write(b"fixture\n")
            return subprocess.CompletedProcess(argv, 0, stdout=HOST.encode({"v": 1, "receiver": self.receiver}))

        with mock.patch.object(HOST.subprocess, "run", side_effect=prepare):
            self.assertEqual(HOST.install_first(self.paths, self.root, 1001, 1001,
                                                "/opt/louiselm/bin/capture", self.writer), self.receiver)
            self.assertEqual(HOST.install_first(self.paths, self.alias, 1001, 1001,
                                                "/opt/louiselm/bin/capture", self.writer), self.receiver)
        self.assertEqual(len(calls), 2)
        self.assertEqual(json.loads((self.base / HOST.CATALOG).read_bytes())["receiver"], self.receiver)

    def test_missing_catalog_never_reuses_a_prepared_candidate_as_fresh_setup(self):
        def prepare(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 0, stdout=HOST.encode({"v": 1, "receiver": self.receiver}))

        with mock.patch.object(HOST.subprocess, "run", side_effect=prepare):
            HOST.install_first(self.paths, self.root, 1001, 1001, "/opt/louiselm/bin/capture", self.writer)
        catalog = self.base / HOST.CATALOG
        catalog.unlink()
        reply = subprocess.CompletedProcess([], 0, stdout=HOST.encode({"v": 1, "receiver": self.receiver}))
        with mock.patch.object(HOST.subprocess, "run", return_value=reply) as run:
            with self.assertRaisesRegex(ValueError, "requires recovery"):
                HOST.install_first(self.paths, self.root, 1001, 1001,
                                   "/opt/louiselm/bin/capture", self.writer)
        run.assert_not_called()
        self.assertFalse(catalog.exists())

    def test_existing_catalog_without_candidate_records_never_invokes_the_writer(self):
        reply = subprocess.CompletedProcess([], 0, stdout=HOST.encode({"v": 1, "receiver": self.receiver}))
        with mock.patch.object(HOST.subprocess, "run", return_value=reply):
            HOST.install_first(self.paths, self.root, 1001, 1001, "/opt/louiselm/bin/capture", self.writer)
        with mock.patch.object(HOST.subprocess, "run", return_value=reply) as run:
            with self.assertRaisesRegex(ValueError, "requires recovery"):
                HOST.install_first(self.paths, self.root, 1001, 1001,
                                   "/opt/louiselm/bin/capture", self.writer)
        run.assert_not_called()

    def test_first_install_refuses_a_known_existing_receiver(self):
        tls = self.root / "tls"
        tls.mkdir()
        (tls / "receiver-key.pem").write_text("prior key", encoding="utf-8")
        with mock.patch.object(HOST.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "existing.*recovery"):
                HOST.install_first(self.paths, self.root, 1001, 1001,
                                   "/opt/louiselm/bin/capture", self.writer)
        run.assert_not_called()
        self.assertEqual((tls / "receiver-key.pem").read_text(encoding="utf-8"), "prior key")
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_first_install_cli_activates_the_installed_registration_socket(self):
        calls = []

        def run(argv, **kwargs):
            calls.append(argv)
            if argv[0] == str(self.base / HOST.WRITER):
                return subprocess.CompletedProcess(argv, 0, stdout=HOST.encode({"v": 1, "receiver": self.receiver}))
            return subprocess.CompletedProcess(argv, 0)

        arguments = ["phone-host.py", "install-first", str(self.root), "--operator", "fixture",
                     "--capture-binary", "/opt/louiselm/bin/capture", "--writer-binary", str(self.writer)]
        with mock.patch.object(sys, "argv", arguments), mock.patch.object(HOST, "Paths", return_value=self.paths), \
                mock.patch.object(HOST.os, "geteuid", return_value=0), mock.patch.object(HOST, "current"), \
                mock.patch.object(HOST.pwd, "getpwnam", return_value=SimpleNamespace(pw_uid=1001, pw_gid=1001)), \
                mock.patch.object(HOST.subprocess, "run", side_effect=run):
            self.assertEqual(HOST.main(), 0)
        self.assertIn(("/usr/bin/systemctl", "enable", "--now", "louiselm-phone-registration.socket"), calls)

    def test_managed_setup_preserves_and_refuses_an_unsafe_private_state(self):
        state = self.base / "var/lib/louiselm-phone-authority"
        state.mkdir(parents=True, mode=0o755)
        state.parent.chmod(0o755)
        state.parent.parent.chmod(0o755)
        (state / "prior").write_text("keep", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "private state"):
            self.install_managed(self.root, self.receiver)
        self.assertEqual((state / "prior").read_text(encoding="utf-8"), "keep")
        self.assertEqual(state.stat().st_mode & 0o777, 0o755)
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_managed_refresh_refuses_a_reparented_capture_before_mounting(self):
        self.install_managed(self.root, self.receiver)
        moved = self.base / "moved-capture"
        self.root.rename(moved)
        self.root.symlink_to(moved, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "capture parent"):
            HOST.refresh(self.paths, require_pid_one=False)
        self.attach.assert_not_called()
        self.assertFalse((self.base / HOST.REFERENCE).exists())

    def test_managed_catalog_refuses_the_same_capture_and_parent_handle(self):
        self.install_managed(self.root, self.receiver)
        path = self.base / HOST.CATALOG
        record = json.loads(path.read_bytes())
        record["owner_parent"] = {name: record[name] for name in ("filesystem_uuid", "handle_type", "handle")}
        path.chmod(0o644)
        path.write_bytes(HOST.encode(record))
        path.chmod(0o444)
        with self.assertRaisesRegex(ValueError, "catalog is invalid"):
            HOST.refresh(self.paths, require_pid_one=False)
        self.attach.assert_not_called()

    def test_managed_catalog_refuses_a_privileged_operator(self):
        self.install_managed(self.root, self.receiver)
        path = self.base / HOST.CATALOG
        record = json.loads(path.read_bytes())
        record["operator_uid"] = 0
        path.chmod(0o644)
        path.write_bytes(HOST.encode(record))
        path.chmod(0o444)
        with self.assertRaisesRegex(ValueError, "catalog is invalid"):
            HOST.refresh(self.paths, require_pid_one=False)

    def test_managed_install_refuses_an_unrepresentable_operator_before_publication(self):
        with self.assertRaisesRegex(ValueError, "operator"):
            HOST.install(self.paths, self.root, self.receiver, 4294967295, 1001,
                         "/opt/louiselm/bin/capture", pin_owner_parent=True, writer_binary=self.writer)
        self.assertFalse((self.base / HOST.CATALOG).exists())

    def test_managed_refresh_does_not_mount_when_parent_cannot_be_reopened(self):
        self.install_managed(self.root, self.receiver)
        with mock.patch.object(HOST, "open_registered_parent", side_effect=OSError(errno.ESTALE, "stale parent")):
            with self.assertRaisesRegex(OSError, "stale parent"):
                HOST.refresh(self.paths, require_pid_one=False)
        self.attach.assert_not_called()
        self.assertFalse((self.base / HOST.REFERENCE).exists())

    def test_managed_install_refuses_without_a_private_same_filesystem_stage(self):
        with mock.patch.object(HOST, "open_filesystem_stage", side_effect=ValueError("no trusted stage")):
            with self.assertRaisesRegex(ValueError, "no trusted stage"):
                self.install_managed(self.root, self.receiver)
        self.assertFalse((self.base / HOST.CATALOG).exists())

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
