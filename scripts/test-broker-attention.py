#!/usr/bin/env python3
"""Disposable-VM gate: actual capture CLI, installer, two UIDs, no live credentials."""

from contextlib import contextmanager
import faulthandler
import json
import os
from pathlib import Path
import pwd
import shutil
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time
import unittest


REPO = Path(__file__).resolve().parents[1]
RECEIVER = Path("/etc/louiselm-capture-broker.json")
SENDER = Path("/etc/louiselm-broker-attention.json")
TMPFILES = Path("/etc/tmpfiles.d/louiselm-broker-attention.conf")
RUNTIME = Path("/run/louiselm-attention")
STATE = Path("/var/lib/louiselm-attention")
AUTHORITY = Path("/usr/local/lib/louiselm/launcher/public-config.json")
TOKEN = STATE / "producer-capability"


@contextmanager
def diagnostics(timeout=90):
    """Report static phases and one stack before CI's unchanged 120s watchdog."""
    started = time.monotonic()

    def phase(label):
        print(f"[broker-attention] {time.monotonic() - started:.3f}s {label}",
              file=sys.stderr, flush=True)

    # Stack locations only: no locals, credentials, requests or child output.
    faulthandler.dump_traceback_later(timeout, repeat=False, exit=False)
    try:
        yield phase
    finally:
        faulthandler.cancel_dump_traceback_later()


def command(*args, **options):
    result = subprocess.run(args, check=False, text=True, capture_output=True, timeout=20, **options)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr}")
    return result


def mount_private_etc(root):
    """Keep fixture account and provisioning writes off the machine's /etc."""
    # Eagerly copying the runner's unrelated configuration dominated CI setup.
    # OverlayFS preserves the full view and copies only files the fixture changes.
    upper, work = root / "etc-upper", root / "etc-work"
    upper.mkdir(mode=0o755)
    work.mkdir(mode=0o700)
    command("mount", "-t", "overlay", "overlay", "-o",
            f"lowerdir=/etc,upperdir={upper},workdir={work}", "/etc")


def client(kind):
    config = json.loads(SENDER.read_bytes())
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
        peer.settimeout(3)
        peer.connect(config["socket"])
        _, uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        assert uid == config["receiver_uid"]
        token = "wrong" if kind == "wrong" else TOKEN.read_text()
        projection = json.loads((REPO / "tests/fixtures/broker_attention_projection.json").read_bytes())
        request = {"type": "project", "request_id": "projection-1", "projection": projection, "capability": token}
        try:
            peer.sendall((json.dumps(request) + "\n").encode())
            reply = peer.makefile("rb").readline(4096)
        except (ConnectionResetError, BrokenPipeError):
            reply = b""
        if kind == "unknown":
            assert not reply
        elif kind == "wrong":
            assert json.loads(reply)["type"] == "mutation_error"
        else:
            result = json.loads(reply)
            assert result["type"] == "projection_result"
            assert result["result"]["sequence"] == 1
            assert result["result"]["applied"] == (kind == "first")


@unittest.skipUnless(os.environ.get("LOUISELM_REQUIRE_BROKER_ATTENTION") == "1",
                     "requires explicit disposable VM privileged gate")
class InstalledAttention(unittest.TestCase):
    def setUp(self):
        self.phase = self.enterContext(diagnostics())

    def test_installer_and_actual_receiver_across_identities_and_restart(self):
        self.phase("validate disposable environment")
        self.assertEqual(os.geteuid(), 0)
        self.assertNotEqual(os.readlink("/proc/self/ns/mnt"), os.readlink("/proc/1/ns/mnt"),
                            "run under unshare --mount --propagation private")
        binary = Path(os.environ["LOUISELM_TEST_CAPTURE"])
        self.assertTrue(binary.is_file())
        accounts = []
        process = None
        created_parents = []
        self.phase("create scratch directory")
        with tempfile.TemporaryDirectory(prefix="louiselm-attention-", dir="/var/tmp") as temporary:
            root = Path(temporary)
            root.chmod(0o711)
            # Runner checkouts may be below a private home directory.
            self.phase("copy capture binary")
            shutil.copyfile(binary, root / "louiselm-capture")
            binary = root / "louiselm-capture"
            binary.chmod(0o555)
            self.phase("copy client fixture")
            client_script = root / "scripts/test-broker-attention.py"
            fixture = root / "tests/fixtures/broker_attention_projection.json"
            for path in (client_script.parent, fixture.parent.parent, fixture.parent):
                path.mkdir(mode=0o755)
                path.chmod(0o755)
            for source, target in ((Path(__file__), client_script),
                                   (REPO / "tests/fixtures/broker_attention_projection.json", fixture)):
                shutil.copyfile(source, target)
                target.chmod(0o644)
            # Hide machine provisioning only in this test's private namespace.
            self.phase("mount private /etc")
            mount_private_etc(root)
            self.phase("hide existing provisioning")
            for path in (RECEIVER, SENDER, TMPFILES):
                path.unlink(missing_ok=True)
            for target in ("/usr/local/lib", "/run", "/var/lib"):
                self.phase(f"mount private {target}")
                command("mount", "-t", "tmpfs", "-o", "mode=0755", "tmpfs", target)
            try:
                for name in ("louiselm-attn-broker-gate", "louiselm-attn-receiver-gate"):
                    self.phase("create fixture account")
                    command("useradd", "--system", "--user-group", "--no-create-home",
                            "--home-dir", "/nonexistent", "--shell", "/usr/sbin/nologin", name)
                    accounts.append(pwd.getpwnam(name))
                broker, receiver = accounts
                self.phase("prepare installed identities and data")
                for parent in reversed(AUTHORITY.parents):
                    if not parent.exists():
                        parent.mkdir(mode=0o755)
                        created_parents.append(parent)
                # Only installed identity fields consumed by the provisioner.
                AUTHORITY.write_text(json.dumps({"broker_uid": broker.pw_uid, "broker_gid": broker.pw_gid,
                                                "operator_uid": receiver.pw_uid, "operator_gid": receiver.pw_gid}))
                AUTHORITY.chmod(0o444)
                data = root / "receiver"
                data.mkdir(mode=0o700)
                os.chown(data, receiver.pw_uid, receiver.pw_gid)
                env = {"PATH": "/usr/bin:/bin", "HOME": str(data),
                       "LOUISELM_ATTENTION_ENABLED": "true",
                       "LOUISELM_CAPTURE_CONFIG_DIR": str(data / "config"),
                       "LOUISELM_CAPTURE_DATA_DIR": str(data / "data"),
                       "LOUISELM_CAPTURE_STATE_DIR": str(data / "state")}
                observer = data / "state/louiselm/workflow/attention.sock"

                def start(wait_for):
                    log = tempfile.TemporaryFile()
                    child = subprocess.Popen(["setpriv", "--reuid", str(receiver.pw_uid),
                                              "--regid", str(receiver.pw_gid), "--clear-groups", str(binary), "serve"],
                                             env=env, stdout=log, stderr=log)
                    deadline = time.monotonic() + 10
                    try:
                        while not wait_for.exists():
                            self.assertIsNone(child.poll(), "capture-service exited before readiness")
                            self.assertLess(time.monotonic(), deadline, "capture-service readiness timed out")
                            time.sleep(0.05)
                    except BaseException:
                        child.terminate()
                        child.wait(timeout=5)
                        log.seek(0)
                        sys.stderr.write(log.read().decode(errors="replace"))
                        log.close()
                        raise
                    log.close()
                    return child

                self.phase("start receiver with unset policy")
                process = start(observer)
                self.assertFalse((RUNTIME / "project.sock").exists(), "unset case must remain disabled")
                self.assertEqual(stat.S_IMODE(observer.stat().st_mode), 0o600)
                self.phase("stop receiver with unset policy")
                process.terminate()
                process.wait(timeout=5)
                installer = REPO / "scripts/install-broker-attention.py"
                self.phase("provision Attention")
                command(sys.executable, str(installer))
                original = {path: (path.read_bytes(), path.stat().st_ino) for path in (TOKEN, RECEIVER, SENDER, TMPFILES)}
                self.phase("verify idempotent provisioning")
                command(sys.executable, str(installer))
                self.assertEqual(original, {path: (path.read_bytes(), path.stat().st_ino) for path in original})
                self.assertEqual((TOKEN.stat().st_uid, stat.S_IMODE(TOKEN.stat().st_mode)), (broker.pw_uid, 0o400))
                self.assertEqual((RUNTIME.stat().st_uid, stat.S_IMODE(RUNTIME.stat().st_mode)), (receiver.pw_uid, 0o711))
                self.phase("start provisioned receiver")
                process = start(RUNTIME / "project.sock")
                for kind in ("wrong", "first", "retry"):
                    self.phase(f"broker client {kind}")
                    command("setpriv", "--reuid", str(broker.pw_uid), "--regid", str(broker.pw_gid),
                            "--clear-groups", sys.executable, str(client_script), "--client", kind)
                self.phase("unknown client")
                command(sys.executable, str(client_script), "--client", "unknown")
                self.phase("stop provisioned receiver")
                process.terminate()
                process.wait(timeout=5)
                (RUNTIME / "project.sock").unlink()
                self.phase("restart receiver for replay")
                process = start(RUNTIME / "project.sock")
                self.phase("replay client")
                command("setpriv", "--reuid", str(broker.pw_uid), "--regid", str(broker.pw_gid),
                        "--clear-groups", sys.executable, str(client_script), "--client", "retry")
                self.phase("stop replay receiver")
                process.terminate()
                process.wait(timeout=5)
                (RUNTIME / "project.sock").unlink()
                policy = json.loads(original[RECEIVER][0])
                for changed in ({**policy, "unknown": True}, {**policy, "broker_uid": 0},
                                {**policy, "socket": "/run/elsewhere.sock"}):
                    self.phase("reject invalid receiver policy")
                    RECEIVER.write_text(json.dumps(changed))
                    refused = subprocess.run(["setpriv", "--reuid", str(receiver.pw_uid),
                                              "--regid", str(receiver.pw_gid), "--clear-groups", str(binary), "serve"],
                                             env=env, capture_output=True, text=True, timeout=5)
                    self.assertNotEqual(refused.returncode, 0)
                    self.assertFalse((RUNTIME / "project.sock").exists())
                RECEIVER.write_bytes(original[RECEIVER][0])
                # Permission drift must refuse re-provisioning, never rotate a key.
                self.phase("reject permission drift")
                TOKEN.chmod(0o444)
                refused = subprocess.run([sys.executable, str(installer)], capture_output=True, text=True, timeout=10)
                self.assertNotEqual(refused.returncode, 0)
                self.assertEqual(TOKEN.read_bytes(), original[TOKEN][0])
                self.assertNotIn(original[TOKEN][0].decode(), refused.stdout + refused.stderr)
            finally:
                self.phase("cleanup receiver")
                if process is not None and process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
                self.phase("remove fixture provisioning")
                for path in (RECEIVER, SENDER, TMPFILES, AUTHORITY):
                    path.unlink(missing_ok=True)
                for path in (RUNTIME, STATE):
                    if path.exists():
                        shutil.rmtree(path)
                for parent in reversed(created_parents):
                    parent.rmdir()
                for account in reversed(accounts):
                    self.phase("delete fixture account")
                    command("userdel", account.pw_name)
                self.phase("unmount private /etc")
                command("umount", "/etc")
                self.phase("remove scratch directory")
        self.phase("complete")


@unittest.skipUnless(os.environ.get("LOUISELM_REQUIRE_BROKER_ATTENTION") == "1",
                     "requires explicit disposable VM privileged gate")
class InstalledEtcIsolation(unittest.TestCase):
    def test_private_etc_does_not_copy_unrelated_files_or_modify_lower(self):
        self.assertEqual(os.geteuid(), 0)
        self.assertNotEqual(os.readlink("/proc/self/ns/mnt"), os.readlink("/proc/1/ns/mnt"))
        with tempfile.TemporaryDirectory(prefix="louiselm-etc-", dir="/var/tmp") as temporary:
            root = Path(temporary)
            lower = root / "lower"
            lower.mkdir(mode=0o755)
            account = lower / "passwd"
            account.write_text("fixture account database\n")
            account.chmod(0o644)
            (lower / "account-link").symlink_to("passwd")
            (lower / "existing-policy").write_text("machine provisioning\n")
            # Unrelated runner files must not be read or materialized at setup.
            os.mkfifo(lower / "unrelated-pipe", 0o600)
            command("mount", "--bind", str(lower), "/etc")
            mounted = False
            try:
                mount_private_etc(root)
                mounted = True
                self.assertEqual(Path("/etc/passwd").read_text(), account.read_text())
                self.assertEqual(stat.S_IMODE(Path("/etc/passwd").stat().st_mode), 0o644)
                self.assertEqual(os.readlink("/etc/account-link"), "passwd")
                self.assertTrue(stat.S_ISFIFO(Path("/etc/unrelated-pipe").stat().st_mode))
                Path("/etc/passwd").write_text("private fixture account\n")
                Path("/etc/provisioned").write_text("private fixture configuration\n")
                Path("/etc/existing-policy").unlink()
                self.assertEqual(account.read_text(), "fixture account database\n")
                self.assertFalse((lower / "provisioned").exists())
                self.assertEqual((lower / "existing-policy").read_text(), "machine provisioning\n")
            finally:
                if mounted:
                    command("umount", "/etc")
                command("umount", "/etc")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--client"]:
        client(sys.argv[2])
    else:
        unittest.main()
