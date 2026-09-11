#!/usr/bin/env python3
"""Isolated operator-command tests; no real state, network or credentials."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


SCRIPT = Path(__file__).with_name("acp-log-backup")


class BackupTests(unittest.TestCase):
    def test_unit_path_reaches_user_installed_runtime(self):
        # louiselm-0ols: observed user-manager PATH excludes ~/.local/bin.
        units = SCRIPT.parent.parent / "contrib/systemd"
        for name in ("louiselm-acp-backup.service", "louiselm-acp-cloud-copy.service"):
            self.assertIn("Environment=PATH=%h/.local/bin:", (units / name).read_text())

    def test_cloud_copy_waits_for_local_backup(self):
        unit = (SCRIPT.parent.parent / "contrib/systemd" /
                "louiselm-acp-cloud-copy.service").read_text()
        self.assertIn("After=louiselm-acp-backup.service", unit)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="acp-backup-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "state" / "acp-llm-adapter"
        self.source.mkdir(parents=True)
        self.log = self.source / "log.jsonl"
        self.log.write_bytes(b'{"synthetic":true}\n')
        self.destination = self.root / "backup"
        self.key = self.root / "password"
        self.key.write_text("synthetic-test-password\n")
        self.key.chmod(0o600)
        self.config = self.root / "config.json"
        self.settings = dict(source=str(self.source), destination=str(self.destination),
                             password_file=str(self.key))
        self.save_config()
        self.env = dict(os.environ, XDG_STATE_HOME=str(self.root / "state"),
                        XDG_CONFIG_HOME=str(self.root / "unset-config"))

    def save_config(self):
        self.config.write_text(json.dumps(self.settings))
        self.config.chmod(0o600)

    def run_command(self, *args, config=True, ok=True, timeout=30):
        command = [sys.executable, str(SCRIPT)]
        if config:
            command += ["--config", str(self.config)]
        result = subprocess.run(command + list(args), env=self.env,
                                capture_output=True, text=True, timeout=timeout)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def test_unconfigured_is_inert(self):
        result = self.run_command("run", config=False)
        self.assertEqual(json.loads(result.stdout)["state"], "disabled")
        self.assertFalse(self.destination.exists())
        self.assertFalse((self.root / "unset-config").exists())

    def test_unknown_config_is_rejected_without_writes(self):
        self.settings["typo"] = True
        self.save_config()
        self.assertIn("config", self.run_command("run", ok=False).stderr)
        self.assertFalse(self.destination.exists())

    def test_destination_cannot_overlap_source_or_state(self):
        for destination in [self.source, self.source / "backup", self.source.parent,
                            self.source.parent / "backup", self.root]:
            with self.subTest(destination=destination):
                self.settings["destination"] = str(destination)
                self.save_config()
                self.assertIn("overlap", self.run_command("init", ok=False).stderr)
        self.assertEqual(self.log.read_bytes(), b'{"synthetic":true}\n')

    def test_public_password_is_refused(self):
        self.key.chmod(0o644)
        self.assertIn("private", self.run_command("init", ok=False).stderr)
        self.assertFalse(self.destination.exists())

    def test_relative_destination_is_refused(self):
        self.settings["destination"] = "relative"
        self.save_config()
        self.assertIn("absolute", self.run_command("init", ok=False).stderr)

    def test_config_errors_are_collected_together(self):
        self.settings.update(source=42, destination="relative", typo=True)
        self.save_config()
        result = self.run_command("init", ok=False)
        for message in ("source", "destination", "unknown config keys"):
            self.assertIn(message, result.stderr)
        self.assertFalse(self.destination.exists())

    def test_key_inside_state_is_refused(self):
        self.settings["password_file"] = str(self.source / "password")
        self.save_config()
        self.assertIn("overlap", self.run_command("init", ok=False).stderr)

    def test_missing_source_does_not_create_repository(self):
        shutil.rmtree(self.source)
        self.assertIn("source", self.run_command("run", ok=False).stderr)
        self.assertFalse((self.destination / "repository").exists())

    def test_empty_source_is_refused(self):
        self.log.unlink()
        (self.source / "metadata.json").write_text("{}")
        self.assertIn("empty", self.run_command("run", ok=False).stderr)
        self.assertFalse((self.destination / "repository").exists())

    def test_excluded_archive_cannot_satisfy_nonempty_source_guard(self):
        self.log.unlink()
        archive = self.source / "proxy/recovered-partials"
        archive.mkdir(parents=True)
        (archive / "fragment.jsonl").write_text('{"excluded":true}\n')
        self.assertIn("empty", self.run_command("run", ok=False).stderr)

    def test_existing_restore_target_is_never_modified(self):
        target = self.root / "restore"
        target.mkdir()
        sentinel = target / "keep"
        sentinel.write_bytes(b"keep")
        result = self.run_command("restore", "a" * 64, str(target), ok=False)
        self.assertIn("target", result.stderr)
        self.assertEqual(sentinel.read_bytes(), b"keep")

    def test_missing_explicit_config_is_an_error(self):
        self.config.unlink()
        self.assertIn("config", self.run_command("run", ok=False).stderr)

    def fake_restic(self):
        fake_bin = self.root / "bin"
        fake_bin.mkdir()
        fake = fake_bin / "restic"
        # Process-boundary fault fixture; summary fields also exercised against
        # real Restic 0.19.1 in test_real_encrypted_backup_deletion_and_restore.
        fake.write_text("#!/usr/bin/env python3\n" + '''
import json, os, sys
from pathlib import Path
if "backup" in sys.argv:
    if os.environ.get("TEST_CHANGE"):
        with Path("log.jsonl").open("ab") as log:
            log.write(b'{"append":true}\\n')
    if os.environ.get("TEST_EXIT"):
        print("PRIVATE-SENTINEL", file=sys.stderr)
        sys.exit(int(os.environ["TEST_EXIT"]))
    print(json.dumps({"message_type": "summary", "snapshot_id": "a" * 64,
                      "total_files_processed": 1,
                      "total_bytes_processed": 0 if os.environ.get("TEST_EMPTY") else 19}))
elif "cat" in sys.argv:
    print(json.dumps({"id": "b" * 64}))
elif "snapshots" in sys.argv:
    print(os.environ["TEST_SNAPSHOTS"])
elif "check" in sys.argv and os.environ.get("TEST_CHECK_EXIT"):
    print("PRIVATE-SENTINEL", file=sys.stderr)
    sys.exit(int(os.environ["TEST_CHECK_EXIT"]))
''')
        fake.chmod(0o700)
        self.env["PATH"] = str(fake_bin) + os.pathsep + self.env["PATH"]
        repository = self.destination / "repository"
        repository.mkdir(parents=True)
        self.destination.chmod(0o700)
        (repository / "config").write_text("synthetic process fixture")

    def test_failed_or_incomplete_backup_preserves_previous_verification(self):
        self.fake_restic()
        first = json.loads(self.run_command("run").stdout)["last_verified"]
        for code in (1, 3, 10, 11):
            with self.subTest(code=code):
                self.env["TEST_EXIT"] = str(code)
                result = self.run_command("run", ok=False)
                self.assertNotIn("PRIVATE-SENTINEL", result.stdout + result.stderr)
                status = json.loads(self.run_command("status").stdout)
                self.assertEqual(status["last_verified"], first)
                self.assertEqual(status["last_attempt"]["state"], "failed")

    def test_failed_integrity_check_does_not_advance_verified_status(self):
        self.fake_restic()
        first = json.loads(self.run_command("run").stdout)["last_verified"]
        for code in (1, 11):
            with self.subTest(code=code):
                self.env["TEST_CHECK_EXIT"] = str(code)
                result = self.run_command("run", ok=False)
                self.assertIn("check failed", result.stderr)
                self.assertNotIn("PRIVATE-SENTINEL", result.stdout + result.stderr)
                status = json.loads(self.run_command("status").stdout)
                self.assertEqual(status["last_verified"], first)
                self.assertEqual(status["last_attempt"]["state"], "failed")

    def test_only_successfully_checked_snapshots_enter_copy_ledger(self):
        self.fake_restic()
        first = json.loads(self.run_command("run").stdout)
        self.assertEqual(first["verified_snapshots"], [first["last_verified"]["snapshot"]])
        self.env["TEST_CHECK_EXIT"] = "1"
        self.run_command("run", ok=False)
        self.assertEqual(json.loads(self.run_command("status").stdout)["verified_snapshots"],
                         first["verified_snapshots"])

    def test_malformed_snapshot_metadata_is_rejected(self):
        self.fake_restic()
        credentials = self.root / "credentials.json"
        credentials.write_text(json.dumps({"type": "service_account", "client_email":
            "louiselm-acp-backup@louiselm.iam.gserviceaccount.com"}))
        credentials.chmod(0o600)
        self.settings["cloud_credentials_file"] = str(credentials)
        self.save_config()
        valid = {"id": "a" * 64, "tree": "b" * 64,
                 "time": "2026-01-01T12:00:00Z", "paths": ["/synthetic"]}
        for field, invalid in (("time", {}), ("paths", 42)):
            self.env["TEST_SNAPSHOTS"] = json.dumps([{**valid, field: invalid}])
            self.run_command("cloud-list", ok=False)

    def cloud_bridge(self):
        # A process-boundary backend substitute, not a live GCS test. Commands,
        # encryption, copy, original-ID mapping, locks and restores are real.
        real = shutil.which("restic")
        self.assertIsNotNone(real)
        credentials = self.root / "credentials.json"
        credentials.write_text(json.dumps({"type": "service_account", "client_email":
            "louiselm-acp-backup@louiselm.iam.gserviceaccount.com"}))
        credentials.chmod(0o600)
        self.settings["cloud_credentials_file"] = str(credentials)
        self.save_config()
        bridge_dir = self.root / "bridge"
        bridge_dir.mkdir()
        bridge = bridge_dir / "restic"
        bridge.write_text("#!/usr/bin/env python3\n" + '''
import json, os, subprocess, sys
args = sys.argv[1:]
if "backup" in args and os.environ.get("TEST_BACKUP_TIME"):
    args += ["--time", os.environ["TEST_BACKUP_TIME"]]
cloud = args[args.index("--repo") + 1] == "gs:louiselm-acp-backups:/restic"
if cloud:
    assert "GOOGLE_ACCESS_TOKEN" not in os.environ
    assert os.environ["GOOGLE_PROJECT_ID"] == "louiselm"
    assert os.environ["GOOGLE_APPLICATION_CREDENTIALS"] == os.environ["TEST_CREDENTIALS"]
    if os.environ.get("TEST_OFFLINE"):
        print("PRIVATE-SENTINEL", file=sys.stderr)
        sys.exit(1)
    args[args.index("--repo") + 1] = os.environ["TEST_REMOTE"]
    if "copy" in args and os.environ.get("TEST_FULL"):
        print("no space left: PRIVATE-SENTINEL", file=sys.stderr)
        sys.exit(1)
    if "copy" in args and os.environ.get("TEST_SLOW_COPY"):
        args += ["--limit-upload", "32"]
if not cloud and "check" in args and os.environ.get("TEST_LOCAL_CHECK_FAIL"):
    sys.exit(1)
with open(os.environ["TEST_TRACE"], "a") as trace:
    trace.write(json.dumps(args) + "\\n")
result = subprocess.run([os.environ["TEST_REAL_RESTIC"], *args], capture_output=True, text=True)
output = result.stdout
if cloud and "snapshots" in args and os.environ.get("TEST_BAD_RECEIPT"):
    values = json.loads(output)
    for value in values:
        value["original"] = "e" * 64
    output = json.dumps(values)
if cloud and "cat" in args and os.environ.get("TEST_WRONG_REPO"):
    value = json.loads(output)
    value["id"] = "e" * 64
    output = json.dumps(value)
print(output, end="")
print(result.stderr, file=sys.stderr, end="")
if cloud and "copy" in args and os.environ.get("TEST_LOST_RECEIPT"):
    sys.exit(1)  # Durable server write followed by a lost client response.
sys.exit(result.returncode)
''')
        bridge.chmod(0o700)
        self.remote = self.root / "remote"
        self.env.update(PATH=str(bridge_dir) + os.pathsep + self.env["PATH"],
                        TEST_REAL_RESTIC=real, TEST_REMOTE=str(self.remote),
                        TEST_TRACE=str(self.root / "trace.jsonl"),
                        TEST_CREDENTIALS=str(credentials), GOOGLE_ACCESS_TOKEN="PRIVATE-SENTINEL")

    def test_unconfigured_cloud_retry_is_inert(self):
        result = self.run_command("cloud-copy", config=False)
        self.assertEqual(json.loads(result.stdout)["state"], "disabled")
        self.assertFalse(self.destination.exists())
        result = self.run_command("cloud-copy")
        self.assertEqual(json.loads(result.stdout)["state"], "cloud-not-configured")
        self.assertFalse(self.destination.exists())

    def test_cloud_credentials_are_private_and_outside_backup_scope(self):
        credentials = self.root / "credentials.json"
        credentials.write_text(json.dumps({"type": "service_account", "client_email":
            "louiselm-acp-backup@louiselm.iam.gserviceaccount.com"}))
        credentials.chmod(0o644)
        self.settings["cloud_credentials_file"] = str(credentials)
        self.save_config()
        self.assertIn("private", self.run_command("cloud-init", ok=False).stderr)
        credentials.chmod(0o600)
        credentials.write_text('{"type":"authorized_user"}')
        self.assertIn("dedicated", self.run_command("cloud-init", ok=False).stderr)
        self.settings["cloud_credentials_file"] = str(self.source / "credential")
        self.save_config()
        self.assertIn("overlap", self.run_command("cloud-init", ok=False).stderr)
        self.assertFalse(self.destination.exists())

    @unittest.skipUnless(shutil.which("restic"), "restic runtime missing")
    def test_real_cloud_copy_retry_verification_and_source_loss_restore(self):
        self.cloud_bridge()
        self.run_command("init")
        local = json.loads(self.run_command("run").stdout)
        self.log.write_text('{"synthetic":"unverified generation"}\n')
        self.env["TEST_LOCAL_CHECK_FAIL"] = "1"
        self.run_command("run", ok=False)  # Snapshot exists but must not enter the copy ledger.
        del self.env["TEST_LOCAL_CHECK_FAIL"]
        self.run_command("cloud-copy", ok=False)  # Never auto-init the remote.
        self.assertFalse(self.remote.exists())
        self.run_command("cloud-init")
        self.run_command("cloud-init", ok=False)
        self.env["TEST_LOST_RECEIPT"] = "1"
        self.run_command("cloud-copy", ok=False)
        cloud = json.loads(self.run_command("status").stdout)["cloud"]
        self.assertEqual(cloud["last_attempt"]["state"], "failed")
        self.assertNotIn("last_copy", cloud)
        del self.env["TEST_LOST_RECEIPT"]
        copied = json.loads(self.run_command("cloud-copy").stdout)
        receipt = copied["receipts"][local["last_verified"]["snapshot"]]
        self.assertRegex(receipt, r"^[0-9a-f]{64}$")
        self.assertNotIn("last_verified", copied)
        self.assertEqual(len(copied["receipts"]), 1)
        self.assertEqual(len(json.loads(self.run_command("cloud-list").stdout)), 1)
        retried = json.loads(self.run_command("cloud-copy").stdout)
        self.assertEqual(retried["receipts"], copied["receipts"])
        # Offline errors keep the previous acknowledgement, never imply freshness.
        self.env["TEST_OFFLINE"] = "1"
        self.assertNotIn("PRIVATE-SENTINEL", self.run_command("cloud-copy", ok=False).stderr)
        offline = json.loads(self.run_command("status").stdout)["cloud"]
        self.assertEqual(offline["last_copy"], retried["last_copy"])
        del self.env["TEST_OFFLINE"]
        verified = json.loads(self.run_command("cloud-verify", receipt).stdout)
        self.assertEqual(verified["last_verified"]["snapshot"], receipt)
        expected = b'{"synthetic":true}\n'  # The verified generation, not the failed later one.
        shutil.rmtree(self.source)
        shutil.rmtree(self.destination)  # Laptop repository AND receipts lost.
        listed = json.loads(self.run_command("cloud-list").stdout)
        self.assertIn(receipt, [item["snapshot"] for item in listed])
        self.run_command("cloud-preview", receipt)
        target = self.root / "cloud-restore"
        self.run_command("cloud-restore", receipt, str(target))
        self.assertEqual((target / "log.jsonl").read_bytes(), expected)
        self.run_command("cloud-restore", receipt, str(target), ok=False)
        self.assertEqual((self.destination / "cloud-status.json").stat().st_mode & 0o777, 0o600)

    @unittest.skipUnless(shutil.which("restic"), "restic runtime missing")
    def test_cloud_faults_preserve_receipts_and_corruption_blocks_retention(self):
        self.cloud_bridge()
        self.run_command("init")
        self.run_command("run")
        self.run_command("cloud-init")
        copied = json.loads(self.run_command("cloud-copy").stdout)
        receipt = next(iter(copied["receipts"].values()))
        good = json.loads(self.run_command("cloud-verify", receipt).stdout)["last_verified"]
        for flag in ("TEST_FULL", "TEST_BAD_RECEIPT", "TEST_WRONG_REPO"):
            self.env[flag] = "1"
            result = self.run_command("cloud-copy", ok=False)
            self.assertNotIn("PRIVATE-SENTINEL", result.stdout + result.stderr)
            status = json.loads(self.run_command("status").stdout)["cloud"]
            self.assertEqual(status["last_copy"], copied["last_copy"])
            self.assertEqual(status["receipts"], copied["receipts"])
            del self.env[flag]
        self.env["TEST_OFFLINE"] = "1"
        self.run_command("cloud-verify", receipt, ok=False)
        del self.env["TEST_OFFLINE"]
        status = json.loads(self.run_command("status").stdout)["cloud"]
        self.assertEqual(status["verification_attempt"]["state"], "failed")
        self.assertEqual(status["last_verified"], good)
        # Corrupt only an owned disposable encrypted pack, not a production backup.
        pack = next(path for path in (self.remote / "data").rglob("*") if path.is_file())
        pack.chmod(0o600)
        with pack.open("r+b") as handle:
            byte = handle.read(1)
            handle.seek(0)
            handle.write(bytes([byte[0] ^ 255]))
        self.run_command("cloud-verify", receipt, ok=False)
        self.assertEqual(json.loads(self.run_command("status").stdout)["cloud"]["last_verified"], good)
        self.run_command("retention-preview", "local", ok=False)

    @unittest.skipUnless(shutil.which("restic"), "restic runtime missing")
    def test_real_copy_overlap_does_not_fail_local_verification(self):
        # louiselm-l1q8: both live timers fired at 16:00:02; check exited11.
        self.cloud_bridge()
        self.log.write_text(json.dumps({"synthetic": os.urandom(256 * 1024).hex()}) + "\n")
        self.run_command("init")
        first = json.loads(self.run_command("run").stdout)["last_verified"]
        self.run_command("cloud-init")
        self.env["TEST_SLOW_COPY"] = "1"
        with subprocess.Popen([sys.executable, str(SCRIPT), "--config", str(self.config),
                               "cloud-copy"], env=self.env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True) as copying:
            try:
                deadline = time.monotonic() + 15
                while not any((self.destination / "repository/locks").iterdir()):
                    self.assertIsNone(copying.poll(), "copy ended before source lock was observed")
                    self.assertLess(time.monotonic(), deadline, "copy never acquired source lock")
                    time.sleep(0.02)
                self.assertIsNone(copying.poll())
                local = json.loads(self.run_command("run", timeout=90).stdout)
            finally:
                stdout, stderr = copying.communicate(timeout=30)
            self.assertEqual(copying.returncode, 0, stderr)
        self.assertEqual(local["last_attempt"]["state"], "verified")
        self.assertIn(first["snapshot"], json.loads(stdout)["receipts"])
        for line in Path(self.env["TEST_TRACE"]).read_text().splitlines():
            args = json.loads(line)
            self.assertEqual(args[args.index("--retry-lock") + 1], "1m")
            self.assertNotIn("--no-lock", args)
            self.assertNotIn("unlock", args)

    @unittest.skipUnless(shutil.which("restic"), "restic runtime missing")
    def test_cloud_does_not_hold_local_wrapper_lock(self):
        import fcntl
        self.cloud_bridge()
        self.run_command("init")
        self.run_command("run")
        self.run_command("cloud-init")
        with (self.destination / "lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.run_command("cloud-copy")
        with (self.destination / "cloud-lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.run_command("run")
            self.run_command("cloud-copy", ok=False)

    @unittest.skipUnless(shutil.which("restic"), "restic runtime missing")
    def test_retention_review_protects_uncopied_and_rejects_stale_approval(self):
        self.cloud_bridge()
        self.run_command("init")
        ids = []
        for number in range(3):
            self.env["TEST_BACKUP_TIME"] = f"2026-01-01 12:00:0{number}"
            self.log.write_text(json.dumps({"synthetic": number}) + "\n")
            ids.append(json.loads(self.run_command("run").stdout)["last_verified"]["snapshot"])
        self.run_command("cloud-init")
        copied = json.loads(self.run_command("cloud-copy").stdout)
        self.run_command("retention-preview", "local", ok=False)  # No full remote verification yet.
        first_remote = copied["receipts"][ids[0]]
        self.run_command("cloud-verify", first_remote)
        for number in range(3, 5):
            self.env["TEST_BACKUP_TIME"] = f"2026-01-01 12:00:0{number}"
            self.log.write_text(json.dumps({"synthetic": number}) + "\n")
            ids.append(json.loads(self.run_command("run").stdout)["last_verified"]["snapshot"])
        plan = json.loads(self.run_command("retention-preview", "local").stdout)
        self.assertIn(ids[1], plan["remove"])
        for protected in [ids[0], ids[3], ids[4]]:
            self.assertNotIn(protected, plan["remove"])
        remote_plan = json.loads(self.run_command("retention-preview", "cloud").stdout)
        self.assertNotIn(first_remote, remote_plan["remove"])
        self.assertEqual(remote_plan["remove"], [])  # Do not delete copies still retained locally.
        # An intervening generation invalidates an approval, even if removals match.
        self.env["TEST_BACKUP_TIME"] = "2026-01-01 12:00:05"
        self.run_command("run")
        self.assertIn("stale", self.run_command("retention-apply", "local", plan["approval"], ok=False).stderr)
        preview = self.run_command("preview", ids[1])
        self.assertTrue(preview.stdout)
        current = json.loads(self.run_command("retention-preview", "local").stdout)
        applied = json.loads(self.run_command("retention-apply", "local", current["approval"]).stdout)
        self.assertEqual(applied["last_retention"]["removed"], current["remove"])
        self.run_command("preview", ids[1], ok=False)
        for protected in [ids[0], ids[3], ids[4]]:
            self.run_command("preview", protected)
        cloud_plan = json.loads(self.run_command("retention-preview", "cloud").stdout)
        self.assertIn(copied["receipts"][ids[1]], cloud_plan["remove"])
        cloud_applied = json.loads(self.run_command("retention-apply", "cloud", cloud_plan["approval"]).stdout)
        self.assertEqual(cloud_applied["last_retention"]["removed"], cloud_plan["remove"])
        self.run_command("cloud-preview", first_remote)
        # A failed/missing source or offline remote cannot trigger pruning.
        self.log.unlink()
        self.run_command("retention-preview", "local", ok=False)
        self.log.write_text('{"synthetic":true}\n')
        self.env["TEST_OFFLINE"] = "1"
        self.run_command("cloud-copy", ok=False)
        del self.env["TEST_OFFLINE"]
        self.run_command("retention-preview", "cloud", ok=False)

    def test_empty_snapshot_result_preserves_previous_verification(self):
        self.fake_restic()
        first = json.loads(self.run_command("run").stdout)["last_verified"]
        self.env["TEST_EMPTY"] = "1"
        self.assertIn("empty", self.run_command("run", ok=False).stderr)
        self.assertEqual(json.loads(self.run_command("status").stdout)["last_verified"], first)

    def test_append_during_backup_is_declared_non_atomic(self):
        self.fake_restic()
        self.env["TEST_CHANGE"] = "1"
        verified = json.loads(self.run_command("run").stdout)["last_verified"]
        self.assertTrue(verified["source_changed"])
        self.assertEqual(verified["consistency"], "non-atomic-live-source")

    def test_concurrent_operation_is_refused(self):
        import fcntl
        self.destination.mkdir(mode=0o700)
        with (self.destination / "lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.assertIn("another backup", self.run_command("run", ok=False).stderr)

    def test_run_never_initializes_repository_implicitly(self):
        result = self.run_command("run", ok=False)
        self.assertIn("not initialized", result.stderr)
        self.assertFalse((self.destination / "repository").exists())

    def test_managed_repository_cannot_be_a_symlink(self):
        self.destination.mkdir(mode=0o700)
        other = self.root / "unrelated-repository"
        other.mkdir()
        (self.destination / "repository").symlink_to(other)
        result = self.run_command("run", ok=False)
        self.assertIn("symlink", result.stderr)
        self.assertEqual(list(other.iterdir()), [])

    @unittest.skipUnless(shutil.which("restic"), "restic runtime missing")
    def test_real_encrypted_backup_deletion_and_restore(self):
        # Includes direct/proxy metadata and links, never external target bytes.
        other = self.source / "proxy" / "connections" / "connection.jsonl"
        other.parent.mkdir(parents=True)
        other.write_bytes(b'{"connection":"synthetic"}\n')
        (self.source / "metadata.json").write_text('{"synthetic":1}')
        external = self.root / "external"
        external.mkdir()
        (external / "secret.jsonl").write_text("not in scope")
        (self.source / "recovered-partials").symlink_to(external)
        (self.source / "other-link").symlink_to(external)
        expected = {str(p.relative_to(self.source)): p.read_bytes()
                    for p in self.source.rglob("*") if p.is_file() and not p.is_symlink()}
        self.run_command("init")
        first = json.loads(self.run_command("run").stdout)["last_verified"]
        self.assertRegex(first["snapshot"], r"^[0-9a-f]{64}$")
        self.assertEqual(first["consistency"], "non-atomic-live-source")
        self.assertEqual(self.destination.stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.destination / "status.json").stat().st_mode & 0o777, 0o600)
        # Source loss must not prevent reading status or restoring older bytes.
        shutil.rmtree(self.source)
        self.run_command("run", ok=False)
        status = json.loads(self.run_command("status").stdout)
        self.assertEqual(status["last_verified"], first)
        self.assertEqual(status["last_attempt"]["state"], "failed")
        preview = [json.loads(line) for line in self.run_command("preview", first["snapshot"]).stdout.splitlines()]
        nodes = [node["path"] for node in preview if node.get("message_type") == "node"]
        self.assertFalse(any("secret.jsonl" in path for path in nodes))
        self.assertFalse(any("recovered-partials" in path for path in nodes))
        target = self.root / "restored"
        self.run_command("restore", first["snapshot"], str(target))
        self.assertEqual(target.stat().st_mode & 0o777, 0o700)
        for relative, content in expected.items():
            self.assertEqual((target / relative).read_bytes(), content)
        self.assertTrue((target / "other-link").is_symlink())
        self.assertFalse((target / "recovered-partials").exists())
        # A second init is an error, never a repository replacement.
        self.run_command("init", ok=False)
        self.assertEqual(json.loads(self.run_command("status").stdout)["last_verified"], first)
        self.source.mkdir()
        self.log.write_bytes(b'{"after":true}\n')
        pack = next(p for p in (self.destination / "repository/data").rglob("*") if p.is_file())
        pack.chmod(0o600)  # Restic makes packs read-only; corrupt only this fixture.
        with pack.open("r+b") as handle:
            original = handle.read(1)
            handle.seek(0)
            handle.write(bytes([original[0] ^ 255]))
        self.run_command("run", ok=False)
        self.assertEqual(json.loads(self.run_command("status").stdout)["last_verified"], first)


if __name__ == "__main__":
    unittest.main()
