#!/usr/bin/env python3
"""Exercise broker provisioning through PID 1 in an explicitly disposable VM."""

import json
import os
from pathlib import Path
import pwd
import runpy
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest


REPO = Path(__file__).resolve().parents[1]
COMMON = runpy.run_path(str(REPO / "scripts/test-broker-attention.py"))
command = COMMON["command"]
UNIT = Path("/etc/systemd/system/louiselm-capture.service")
POLICY = COMMON["RECEIVER"]
SOCKET = COMMON["RUNTIME"] / "project.sock"


class UnitRendering(unittest.TestCase):
    def test_operator_home_cannot_inject_unit_directives_or_specifiers(self):
        render = runpy.run_path(str(REPO / "scripts/install-broker-attention.py"))["capture_system_unit"]
        for home in ("/", "relative", "/home/../root", "/home/name\nUser=0", "/home/%h", "/home/$USER"):
            with self.subTest(home=home), self.assertRaisesRegex(ValueError, "systemd-safe"):
                render(pwd.struct_passwd(("fixture", "x", 1234, 1234, "", home, "/bin/false")))


@unittest.skipUnless(os.environ.get("LOUISELM_REQUIRE_CAPTURE_SYSTEMD") == "1",
                     "requires an explicitly disposable VM with root and systemd PID 1")
class InstalledCapture(unittest.TestCase):
    def test_hardened_service_preserves_policy_and_peer_identities(self):
        self.assertEqual(os.geteuid(), 0)
        self.assertEqual(Path("/proc/1/comm").read_text().strip(), "systemd")
        self.assertEqual(os.readlink("/proc/self/ns/mnt"), os.readlink("/proc/1/ns/mnt"))
        paths = [COMMON[key] for key in ("RECEIVER", "SENDER", "TMPFILES", "AUTHORITY")]
        for path in [*paths, UNIT, COMMON["RUNTIME"], COMMON["STATE"]]:
            self.assertFalse(path.exists() or path.is_symlink(), f"preserve existing provisioning: {path}")
        self.assertEqual(self.property("LoadState"), "not-found")
        binary = Path(os.environ["LOUISELM_TEST_CAPTURE"]).resolve(strict=True)
        accounts, parents = [], []
        # PrivateTmp hides /tmp and /var/tmp from the real shipped service.
        with tempfile.TemporaryDirectory(prefix="louiselm-capture-gate-", dir="/opt") as temporary:
            root = Path(temporary)
            root.chmod(0o711)
            try:
                for role in ("broker", "receiver"):
                    name = f"lmcg-{role}-{os.getpid()}"
                    command("useradd", "--system", "--user-group", "--no-create-home",
                            "--home-dir", str(root / role), "--shell", "/usr/sbin/nologin", name)
                    accounts.append(pwd.getpwnam(name))
                broker, receiver = accounts
                home = Path(receiver.pw_dir)
                home.mkdir(mode=0o700)
                for relative in (".local/bin", ".config/louiselm", ".local/share/louiselm",
                                 ".local/state/louiselm", "code/louiselm/.beads"):
                    (home / relative).mkdir(parents=True, exist_ok=True)
                shutil.copyfile(binary, home / ".local/bin/louiselm-capture")
                (home / ".local/bin/louiselm-capture").chmod(0o755)
                environment = home / ".config/louiselm/capture.env"
                environment.write_text("LOUISELM_ATTENTION_ENABLED=true\n")
                environment.chmod(0o600)
                for directory, _, files in os.walk(home):
                    os.chown(directory, receiver.pw_uid, receiver.pw_gid)
                    for name in files:
                        os.chown(Path(directory) / name, receiver.pw_uid, receiver.pw_gid)
                for parent in reversed(COMMON["AUTHORITY"].parents):
                    if not parent.exists():
                        parent.mkdir(mode=0o755)
                        parents.append(parent)
                COMMON["AUTHORITY"].write_text(json.dumps({
                    "broker_uid": broker.pw_uid, "broker_gid": broker.pw_gid,
                    "operator_uid": receiver.pw_uid, "operator_gid": receiver.pw_gid,
                }))
                COMMON["AUTHORITY"].chmod(0o444)
                command(sys.executable, str(REPO / "scripts/install-broker-attention.py"))
                self.assertTrue(UNIT.is_file(), "provisioning must install the supported system unit")
                unit_bytes = UNIT.read_bytes()
                command(sys.executable, str(REPO / "scripts/install-broker-attention.py"))
                self.assertEqual(UNIT.read_bytes(), unit_bytes)
                self.assertEqual((UNIT.stat().st_uid, stat.S_IMODE(UNIT.stat().st_mode)), (0, 0o644))
                command("systemd-analyze", "verify", str(UNIT))
                command("systemctl", "daemon-reload")
                observer = home / ".local/state/louiselm/workflow/attention.sock"

                client = root / "scripts/test-broker-attention.py"
                fixture = root / "tests/fixtures/broker_attention_projection.json"
                for source, target in ((REPO / "scripts/test-broker-attention.py", client),
                                       (REPO / "tests/fixtures/broker_attention_projection.json", fixture)):
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, target)
                    target.chmod(0o644)

                def as_broker(*args):
                    return command("setpriv", "--reuid", str(broker.pw_uid),
                                   "--regid", str(broker.pw_gid), "--clear-groups", *args)

                for kinds in (("wrong", "first", "retry"), ("retry",)):
                    self.start(SOCKET)
                    pid = self.property("MainPID")
                    self.assertEqual(os.readlink(f"/proc/{pid}/ns/user"), os.readlink("/proc/1/ns/user"))
                    self.assertEqual(self.property("User"), str(receiver.pw_uid))
                    self.assertEqual(self.property("ProtectSystem"), "strict")
                    self.assertEqual(self.property("ProtectHome"), "read-only")
                    self.assertEqual(self.property("NoNewPrivileges"), "yes")
                    mounts = {fields[4]: fields[5].split(",") for line in Path(f"/proc/{pid}/mountinfo").read_text().splitlines()
                              if (fields := line.split())}
                    self.assertIn("ro", mounts["/"])
                    self.assertIn("rw", mounts[str(home / ".local/state/louiselm")])
                    self.assertEqual(stat.S_IMODE(observer.stat().st_mode), 0o600)
                    for kind in kinds:
                        as_broker(sys.executable, str(client), "--client", kind)
                    command(sys.executable, str(client), "--client", "unknown")
                    as_broker(sys.executable, "-c",
                              "import socket,sys\n"
                              "try: socket.socket(socket.AF_UNIX).connect(sys.argv[1])\n"
                              "except PermissionError: sys.exit(0)\n"
                              "sys.exit(1)\n", str(observer))
                    self.stop(observer)

                policy = POLICY.read_bytes()
                POLICY.unlink()
                self.start(observer)
                self.assertFalse(SOCKET.exists(), "absent policy must keep projection disabled")
                self.stop(observer)
                for content, uid, mode, reason in (
                    (b"{}", 0, 0o644, "broker Attention policy is malformed"),
                    (policy, receiver.pw_uid, 0o644, "broker Attention policy is not a trusted regular file"),
                    (policy, 0, 0o666, "broker Attention policy is not a trusted regular file"),
                ):
                    POLICY.write_bytes(content)
                    os.chown(POLICY, uid, 0)
                    POLICY.chmod(mode)
                    command("systemctl", "start", UNIT.name)
                    deadline = time.monotonic() + 5
                    while self.property("ExecMainStatus") == "0":
                        self.assertLess(time.monotonic(), deadline, "invalid policy was not refused")
                        time.sleep(0.05)
                    self.assertEqual(self.property("ExecMainStatus"), "1")
                    logs = command("journalctl", "--no-pager", "-o", "cat",
                                   f"_SYSTEMD_INVOCATION_ID={self.property('InvocationID')}").stdout
                    self.assertIn(reason, logs)
                    self.assertFalse(SOCKET.exists())
                    self.stop(observer)
            finally:
                if UNIT.exists():
                    command("systemctl", "stop", UNIT.name)
                    UNIT.unlink()
                    command("systemctl", "daemon-reload")
                    subprocess.run(["systemctl", "reset-failed", UNIT.name], check=False, capture_output=True)
                for path in paths:
                    path.unlink(missing_ok=True)
                for path in (COMMON["RUNTIME"], COMMON["STATE"]):
                    if path.exists():
                        shutil.rmtree(path)
                for parent in reversed(parents):
                    parent.rmdir()
                for account in reversed(accounts):
                    command("userdel", account.pw_name)

    @staticmethod
    def property(name):
        return command("systemctl", "show", "--value", "-p", name, UNIT.name).stdout.strip()

    def start(self, endpoint):
        command("systemctl", "start", UNIT.name)
        deadline = time.monotonic() + 10
        while not endpoint.exists():
            self.assertIn(self.property("SubState"), ("running", "start"))
            self.assertLess(time.monotonic(), deadline, "service readiness timed out")
            time.sleep(0.05)

    @staticmethod
    def stop(observer):
        command("systemctl", "stop", UNIT.name)
        SOCKET.unlink(missing_ok=True)
        observer.unlink(missing_ok=True)


if __name__ == "__main__":
    unittest.main()
