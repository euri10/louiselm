#!/usr/bin/env python3
"""Exercise shipped system units with PID 1 in a disposable Linux VM.

Substitute only the daemon and installation identity/paths. The separate
installed_daemon_tests Rust gate exercises the measured production executable.
"""

import json
import os
from pathlib import Path
import pwd
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=20).stdout.strip()


def probe():
    assert int(os.environ["LISTEN_PID"]) == os.getpid()
    assert os.environ["LISTEN_FDS"] == "1"
    with socket.socket(fileno=3) as listener:
        assert listener.type == socket.SOCK_SEQPACKET
        assert listener.getsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED) == 1
        while True:
            with listener.accept()[0] as peer:
                _, ancillary, _, _ = peer.recvmsg(128, socket.CMSG_SPACE(12))
                assert any(kind == socket.SCM_CREDENTIALS for _, kind, _ in ancillary)
                peer.send(json.dumps({"pid": os.getpid(), "uid": os.getuid(),
                                      "gid": os.getgid(), "groups": os.getgroups()}).encode())


@unittest.skipUnless(os.environ.get("LOUISELM_REQUIRE_BROKER_SYSTEMD") == "1",
                     "requires explicit disposable VM systemd gate")
class BrokerUnits(unittest.TestCase):
    def test_workspace_expiry_timer_runs_fixed_command_as_root(self):
        self.assertEqual(os.geteuid(), 0)
        source = Path(__file__).resolve().parents[1] / "skills-core/contrib/systemd"
        with tempfile.TemporaryDirectory(prefix="louiselm-expiry-", dir="/run") as temporary:
            root = Path(temporary)
            name = root.name
            sessions, policy = root / "sessions", root / "policy"
            sessions.mkdir()
            policy.mkdir()
            result = sessions / "invocation.json"
            script = root / "probe.py"
            shutil.copyfile(__file__, script)
            units = []
            try:
                for kind in ("service", "timer"):
                    text = (source / f"louiselm-workspace-cleanup.{kind}").read_text()
                    text = text.replace("louiselm-workspace-cleanup", name)
                    text = text.replace("/usr/local/lib/louiselm/current/bin/louiselm-launch",
                                        f"/usr/bin/python3 {script} --cleanup-probe {result}")
                    text = text.replace("/var/lib/louiselm/sessions", str(sessions))
                    text = text.replace("/var/lib/louiselm/broker/authorizations/workspace-retention", str(policy))
                    text = text.replace("OnCalendar=hourly", "OnActiveSec=1s\nAccuracySec=1ms")
                    text = text.replace("RandomizedDelaySec=5m", "RandomizedDelaySec=0")
                    unit = Path("/run/systemd/system") / f"{name}.{kind}"
                    with unit.open("x") as output:
                        output.write(text)
                    units.append(unit)
                run("systemd-analyze", "verify", "--recursive-errors=yes", *map(str, units))
                run("systemctl", "daemon-reload")
                run("systemctl", "enable", "--runtime", "--now", f"{name}.timer")
                deadline = time.monotonic() + 10
                while not result.exists():
                    self.assertLess(time.monotonic(), deadline, "cleanup timer did not invoke the fixed command")
                    time.sleep(0.05)
                self.assertEqual(json.loads(result.read_text()), {"uid": 0, "gid": 0, "arguments": ["cleanup"]})
                self.assertEqual(run("systemctl", "show", "--value", "-p", "Persistent", f"{name}.timer"), "yes")
                self.assertTrue((Path("/run/systemd/system/timers.target.wants") / f"{name}.timer").is_symlink())
            finally:
                if units:
                    if not result.exists():
                        subprocess.run(["journalctl", "--no-pager", "-n", "20", "-u", f"{name}.service"], check=False)
                    run("systemctl", "stop", f"{name}.timer", f"{name}.service")
                    run("systemctl", "disable", "--runtime", f"{name}.timer")
                    for unit in units:
                        unit.unlink()
                    run("systemctl", "daemon-reload")

    def test_provision_activation_crash_restart_and_stop(self):
        self.assertEqual(os.geteuid(), 0)
        self.assertEqual(Path("/proc/1/comm").read_text().strip(), "systemd")
        source = Path(__file__).resolve().parents[1] / "skills-core/contrib/systemd"
        with tempfile.TemporaryDirectory(prefix="louiselm-systemd-", dir="/var/tmp") as temporary:
            root = Path(temporary)
            root.chmod(0o755)
            name = root.name
            runtime = Path("/run") / name
            operator_runtime = Path("/run") / f"{name}-operator"
            state_parent = Path("/var/lib") / name
            state = state_parent / "broker"
            self.assertFalse(runtime.exists())
            self.assertFalse(operator_runtime.exists())
            self.assertFalse(state_parent.exists())
            script = root / "probe.py"
            shutil.copyfile(__file__, script)
            script.chmod(0o644)
            account = pwd.getpwnam("nobody")
            units = []
            try:
                for kind in ("socket", "service"):
                    text = (source / f"louiselm-broker.{kind}").read_text()
                    text = text.replace("louiselm-broker", name)
                    text = text.replace("=louiselm\n", f"={name}\n")
                    text = text.replace("=louiselm-operator\n", f"={name}-operator\n")
                    text = text.replace("=louiselm/broker", f"={name}/broker")
                    text = text.replace("/run/louiselm/", f"{runtime}/")
                    text = text.replace(f"User={name}", "User=nobody")
                    text = text.replace(f"Group={name}", f"Group={account.pw_gid}")
                    text = text.replace("ExecStart=/usr/local/lib/louiselm/current/bin/louiselm-control serve",
                                        f"ExecStart=/usr/bin/python3 {script} --probe")
                    unit = Path("/run/systemd/system") / f"{name}.{kind}"
                    with unit.open("x") as output:
                        output.write(text)
                    units.append(unit)
                run("systemd-analyze", "verify", "--recursive-errors=yes", *map(str, units))
                run("systemctl", "daemon-reload")
                run("systemctl", "enable", "--runtime", f"{name}.socket", f"{name}.service")
                self.assertTrue((Path("/run/systemd/system/sockets.target.wants") / f"{name}.socket").is_symlink())
                self.assertTrue((Path("/run/systemd/system/multi-user.target.wants") / f"{name}.service").is_symlink())
                run("systemctl", "start", f"{name}.socket")
                self.assertEqual((runtime.stat().st_uid, stat.S_IMODE(runtime.stat().st_mode)),
                                 (account.pw_uid, 0o700))
                endpoint = runtime / "control.sock"
                self.assertEqual((endpoint.stat().st_uid, stat.S_IMODE(endpoint.stat().st_mode)),
                                 (account.pw_uid, 0o600))
                inode = endpoint.stat().st_ino

                def connect():
                    with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as client:
                        client.settimeout(10)
                        client.connect(str(endpoint))
                        client.send(b"queued before accept")
                        return json.loads(client.recv(1024))

                first = connect()
                self.assertEqual(first["uid"], account.pw_uid)
                self.assertEqual(first["gid"], account.pw_gid)
                self.assertEqual((operator_runtime.stat().st_uid,
                                  stat.S_IMODE(operator_runtime.stat().st_mode)),
                                 (account.pw_uid, 0o755))
                self.assertTrue(all(group == account.pw_gid for group in first["groups"]))
                self.assertEqual((state.stat().st_uid, stat.S_IMODE(state.stat().st_mode)),
                                 (account.pw_uid, 0o700))
                self.assertEqual(state_parent.stat().st_uid, 0)
                self.assertEqual(stat.S_IMODE(state_parent.stat().st_mode) & 0o022, 0)
                marker = state / "retained"
                marker.write_text("durable state")
                run("systemctl", "kill", "--signal=KILL", "--kill-whom=main", f"{name}.service")
                deadline = time.monotonic() + 10
                while run("systemctl", "show", "--value", "-p", "MainPID", f"{name}.service") in ("0", str(first["pid"])):
                    self.assertLess(time.monotonic(), deadline, "broker must restart without a connection")
                    time.sleep(0.05)
                self.assertNotEqual(connect()["pid"], first["pid"])
                run("systemctl", "stop", f"{name}.service")
                self.assertEqual(endpoint.stat().st_ino, inode)
                self.assertEqual(marker.read_text(), "durable state")
                run("systemctl", "start", f"{name}.service")
                connect()
                self.assertEqual(endpoint.stat().st_ino, inode)
                run("systemctl", "stop", f"{name}.socket", f"{name}.service")
                self.assertFalse(runtime.exists())
                self.assertFalse(operator_runtime.exists())
                self.assertEqual(marker.read_text(), "durable state")
            finally:
                if units:
                    subprocess.run(["journalctl", "--no-pager", "-n", "30", "-u", f"{name}.service"], check=False)
                    run("systemctl", "stop", f"{name}.socket", f"{name}.service")
                    run("systemctl", "disable", "--runtime", f"{name}.socket", f"{name}.service")
                    for unit in units:
                        unit.unlink()
                    run("systemctl", "daemon-reload")
                if state_parent.exists():
                    shutil.rmtree(state_parent)


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--cleanup-probe":
        Path(sys.argv[2]).write_text(json.dumps({"uid": os.getuid(), "gid": os.getgid(), "arguments": sys.argv[3:]}))
    elif sys.argv[1:] == ["--probe"]:
        probe()
    else:
        unittest.main()
