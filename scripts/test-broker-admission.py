#!/usr/bin/env python3
"""Private-VM gate for the real linked CLI, durable broker and read-only UID boundary."""

import json
import os
from pathlib import Path
import pwd
import runpy
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
COMMON = runpy.run_path(str(Path(__file__).with_name("test-broker-attention.py")))
command = COMMON["command"]


@unittest.skipUnless(os.environ.get("LOUISELM_REQUIRE_BROKER_ADMISSION") == "1",
                     "requires explicit disposable VM privileged gate")
class LinkedAdmission(unittest.TestCase):
    def test_cli_outage_restart_and_read_only_evidence(self):
        self.assertEqual(os.geteuid(), 0)
        self.assertNotEqual(os.readlink("/proc/self/ns/mnt"), os.readlink("/proc/1/ns/mnt"))
        builder = runpy.run_path(str(REPO / "scripts/test-skill-requests"))
        broker_binary = os.environ.get("LOUISELM_TEST_ADMISSION_BROKER") or builder["test_binary"]("skills-core", "broker")
        skills_binary = Path(os.environ["LOUISELM_TEST_SKILLS"])
        with tempfile.TemporaryDirectory(prefix="admission-gate-", dir="/var/tmp") as temporary:
            scratch = Path(temporary)
            shutil.copytree("/etc", scratch / "etc", symlinks=True)
            command("mount", "--bind", str(scratch / "etc"), "/etc")
            for target in ("/var/lib", "/run", "/usr/local/lib"):
                command("mount", "-t", "tmpfs", "-o", "mode=0755", "tmpfs", target)
            root = Path("/var/lib/louiselm-admission-gate")
            root.mkdir(mode=0o755)
            for original, name in ((broker_binary, "broker-test"), (skills_binary, "skills")):
                shutil.copyfile(original, root / name)
                (root / name).chmod(0o555)
            source_config = Path("/etc/louiselm-broker-admission.json")
            source_config.unlink(missing_ok=True)
            accounts = []
            server = None
            try:
                for name in ("louiselm-admission-operator", "louiselm-admission-reader"):
                    command("useradd", "--system", "--user-group", "--no-create-home", "--shell", "/usr/sbin/nologin", name)
                    accounts.append(pwd.getpwnam(name))
                operator, broker = accounts
                private = root / "private"
                private.mkdir(mode=0o700)
                os.chown(private, operator.pw_uid, operator.pw_gid)
                state = root / "broker"
                state.mkdir(mode=0o700)
                os.chown(state, broker.pw_uid, broker.pw_gid)
                store = root / "store"
                store.mkdir(mode=0o700)
                os.chown(store, operator.pw_uid, operator.pw_gid)
                runtime = Path("/run/louiselm-operator")
                runtime.mkdir(mode=0o755)
                os.chown(runtime, broker.pw_uid, broker.pw_gid)

                def as_user(account, *args):
                    return command("setpriv", "--reuid", str(account.pw_uid), "--regid", str(account.pw_gid),
                                   "--clear-groups", *map(str, args), env={"PATH": "/usr/bin:/bin", "HOME": str(private)})

                def cli(*args):
                    return json.loads(as_user(operator, root / "skills", *args, "--store", store, "--robot-json").stdout)

                for name in ("primary", "release"):
                    as_user(operator, "ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", private / name)
                cli("trust", "bootstrap", "--primary", private / "primary.pub", "--release", private / "release.pub")
                candidate = private / "candidate"
                candidate.mkdir(mode=0o755)
                os.chown(candidate, operator.pw_uid, operator.pw_gid)
                (candidate / "SKILL.md").write_text("---\nname: skill\ndescription: Test skill.\n---\nBody.\n")
                package = cli("package", candidate)["digest"]
                standalone = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary")
                self.assertNotIn("approval_operation", standalone["payload"])
                self.assertFalse(source_config.exists(), "standalone must not configure linkage")
                with self.assertRaisesRegex(RuntimeError, "not configured"):
                    cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary",
                        "--skill-request", "12345678-1234-4234-8234-123456789abc")
                provenance = store / "provenance.json"
                value = json.loads(provenance.read_bytes())
                value.update(trusted=True, created_by_release="software-key-fixture")
                provenance.write_text(json.dumps(value))  # Test-only provenance, not a production promotion.
                authority = Path("/usr/local/lib/louiselm/launcher/public-config.json")
                authority.parent.mkdir(parents=True, mode=0o755)
                authority.write_text(json.dumps({"operator_uid": operator.pw_uid, "broker_uid": broker.pw_uid, "broker_gid": broker.pw_gid}))
                authority.chmod(0o444)
                installer = REPO / "scripts/install-broker-admission.py"
                command(sys.executable, str(installer), "--store", str(store))
                original = source_config.read_bytes()
                command(sys.executable, str(installer), "--store", str(store))
                self.assertEqual(source_config.read_bytes(), original)
                as_user(broker, "test", "-r", store / "trust/roles.json")
                as_user(broker, "test", "!", "-w", store / "trust/roles.json")
                as_user(broker, "test", "!", "-r", private / "primary")
                (candidate / "SKILL.md").write_text("---\nname: skill\ndescription: Test skill.\n---\nChanged body.\n")
                subsequent = cli("package", candidate)["digest"].replace(":", "-", 1)
                as_user(broker, "test", "-r", store / "packages" / subsequent / "files/SKILL.md")
                as_user(broker, "test", "!", "-w", store / "packages" / subsequent)

                def start(calls):
                    env = {"PATH": "/usr/bin:/bin", "HOME": str(state), "LOUISELM_ADMISSION_STATE": str(state),
                           "LOUISELM_ADMISSION_OPERATOR": str(operator.pw_uid), "LOUISELM_ADMISSION_PACKAGE": package,
                           "LOUISELM_ADMISSION_CALLS": str(calls)}
                    child = subprocess.Popen(["setpriv", "--reuid", str(broker.pw_uid), "--regid", str(broker.pw_gid),
                                              "--clear-groups", str(root / "broker-test"), "--ignored", "--exact",
                                              "skill_requests::installed_linked_admission_server", "--nocapture"], env=env)
                    deadline = time.monotonic() + 10
                    while not (runtime / "inspect.sock").exists():
                        if child.poll() is not None or time.monotonic() > deadline:
                            child.terminate()
                            child.wait(timeout=5)
                            self.fail("broker readiness failed")
                        time.sleep(0.02)
                    return child

                server = start(1)  # Lose broker after preflight, before signing finishes.
                operation = (state / "operation").read_text()
                linked = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary", "--skill-request", operation)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(linked["admission"]["state"], "pending_witness")
                self.assertIsNone(linked["broker_status"])
                self.assertEqual(linked["resolution_error"], "broker_unavailable")
                self.assertEqual(json.loads((state / "status").read_bytes())["outcome"], "pending")
                server = start(2)  # Startup reconciles actual durable evidence; no new signature.
                retry = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "missing-key", "--skill-request", operation)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(retry["admission"], linked["admission"])
                self.assertEqual(retry["broker_status"]["outcome"], "approved")
                as_user(broker, "test", "-r", store / "trust/roles.json")
                as_user(broker, "test", "!", "-w", store / "generations")
                self.assertFalse((store / "pins.jsonl").exists(), "approval never activates supply")
            finally:
                if server is not None and server.poll() is None:
                    server.terminate()
                    server.wait(timeout=5)
                for account in reversed(accounts):
                    command("userdel", account.pw_name)
                command("umount", "/etc")


if __name__ == "__main__":
    unittest.main()
