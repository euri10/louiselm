#!/usr/bin/env python3
"""Exercise first candidate setup and the fail-closed R path in a disposable VM.

This does not establish first origin, native registrar authority, or Send.
"""

import argparse
import array
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import pwd
import shutil
import socket
import subprocess
import sys
import time


BASE = Path("/opt/louiselm-managed-registration-fixture")
SOCKET = Path("/run/louiselm-phone-host/registration.sock")
CATALOG = Path("/var/lib/louiselm-phone-host/catalog.json")
PRIVATE = Path("/var/lib/louiselm-phone-authority")
UNIT = "louiselm-phone-registration.socket"


def run(*args, check=True, env=None):
    result = subprocess.run(args, text=True, capture_output=True, timeout=60, env=env, check=False)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} failed ({result.returncode}): {result.stderr[-1500:]}")
    return result


def operator(*args, state=None, check=True):
    return run("runuser", "-u", "vm", "--", "env",
               f"LOUISELM_CAPTURE_STATE_DIR={state or BASE / 'state'}",
               f"LOUISELM_CAPTURE_DATA_DIR={BASE / 'data'}",
               f"LOUISELM_CAPTURE_CONFIG_DIR={BASE / 'config'}",
               str(BASE / "louiselm-capture"), *args, check=check)


def request(root):
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(10)
            connection.connect(str(SOCKET))
            connection.sendmsg([b"R"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [fd]))])
            data = bytearray()
            while True:
                part = connection.recv(1024)
                if not part:
                    break
                data.extend(part)
                if len(data) > 1024:
                    raise RuntimeError("oversized registration reply")
        return json.loads(data)
    finally:
        os.close(fd)


def journal_cursor():
    journal = run("journalctl", "--no-pager", "-b", "0", "-o", "json", "-n", "1").stdout
    return json.loads(journal.splitlines()[-1])["__CURSOR"]


def assert_origin_refusal(cursor):
    deadline = time.monotonic() + 5
    while True:
        journal = run("journalctl", "--no-pager", "-b", "0", "-o", "json",
                      "--after-cursor", cursor).stdout
        entries = (json.loads(line) for line in journal.splitlines())
        if any(entry.get("_SYSTEMD_UNIT", "").startswith("louiselm-phone-registration@")
               and entry.get("MESSAGE") == "managed phone registration: origin_unavailable"
               for entry in entries):
            return
        assert time.monotonic() < deadline, "writer did not reach the independent-origin refusal"
        time.sleep(0.1)


def setup(capture, writer, installer):
    for path in (BASE, CATALOG.parent, PRIVATE, Path("/usr/local/lib/louiselm-phone-host"),
                 Path("/etc/systemd/system/louiselm-phone-host.service"),
                 Path("/etc/systemd/system/louiselm-phone-registration.socket"),
                 Path("/run/louiselm-phone-host")):
        if path.exists() or path.is_symlink():
            raise RuntimeError(f"fixture refuses existing path: {path}")
    account = pwd.getpwnam("vm")
    BASE.mkdir(mode=0o755)
    for name, source in (("louiselm-capture", capture), ("phone-writer", writer)):
        target = BASE / name
        shutil.copyfile(source, target)
        target.chmod(0o755)
    for name in ("state", "data", "config", "other"):
        path = BASE / name
        path.mkdir(mode=0o755)
        os.chown(path, account.pw_uid, account.pw_gid)
    root = BASE / "state/louiselm/capture"
    root.mkdir(parents=True)
    for path in (root.parent, root):
        os.chown(path, account.pw_uid, account.pw_gid)
    other = BASE / "other/louiselm/capture"
    other.mkdir(parents=True)
    for path in (other.parent, other):
        os.chown(path, account.pw_uid, account.pw_gid)
    os.symlink(BASE / "state", BASE / "state-alias")
    args = (sys.executable, "-I", str(installer), "install-first", str(root),
            "--operator", "vm", "--capture-binary", str(BASE / "louiselm-capture"),
            "--writer-binary", str(BASE / "phone-writer"))
    run(*args)
    catalog = CATALOG.read_bytes()
    setup_record = (PRIVATE / "setup.json").read_bytes()
    candidate = (PRIVATE / "candidate.prepared").read_bytes()
    run(*args)
    assert CATALOG.read_bytes() == catalog
    assert (PRIVATE / "setup.json").read_bytes() == setup_record
    assert (PRIVATE / "candidate.prepared").read_bytes() == candidate
    assert not root.joinpath("tls/receiver-key.pem").exists()
    assert PRIVATE.stat().st_uid == 0 and PRIVATE.stat().st_mode & 0o777 == 0o700
    assert run("runuser", "-u", "vm", "--", "test", "-r", str(PRIVATE / "candidate/receiver-key.pem"),
               check=False).returncode != 0
    changed = list(args)
    changed[4] = str(other)
    assert "requires recovery" in run(*changed, check=False).stderr
    assert CATALOG.read_bytes() == catalog and (PRIVATE / "candidate.prepared").read_bytes() == candidate
    bad_writer = BASE / "untrusted-writer"
    bad_writer.write_bytes(b"fixture")
    bad_writer.chmod(0o775)
    changed = list(args)
    changed[-1] = str(bad_writer)
    assert "untrusted writer" in run(*changed, check=False).stderr
    assert CATALOG.read_bytes() == catalog and (PRIVATE / "candidate.prepared").read_bytes() == candidate
    (BASE / "initial-boot-id").write_text(Path("/proc/sys/kernel/random/boot_id").read_text())
    operator("configure-network", "--profile", "private", "--bind", "10.254.0.1:7391",
             "--url", "https://10.254.0.1:7391")
    print("first setup and repeat: private candidate and exact catalog retained")


def verify(after_reboot):
    if after_reboot:
        previous = (BASE / "initial-boot-id").read_text().strip()
        current = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        assert previous != current, "expected a new guest boot"
    root = BASE / "state/louiselm/capture"
    alias = BASE / "state-alias/louiselm/capture"
    other = BASE / "other/louiselm/capture"
    # These are ordinary request/response checks, not first-origin evidence.
    cursor = journal_cursor()
    assert request(root) == {"v": 1, "result": "unavailable"}
    assert_origin_refusal(cursor)
    assert request(alias) == {"v": 1, "result": "unavailable"}
    assert request(other) == {"v": 1, "result": "root_mismatch"}
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(request, (root, alias)))
    assert results == [{"v": 1, "result": "unavailable"}] * 2
    run("systemctl", "restart", UNIT)
    assert request(root) == {"v": 1, "result": "unavailable"}
    svg = BASE / "state/pairing.svg"
    if not svg.exists():
        operator("pair", "--svg", str(svg))
    assert svg.is_file()
    operator("status")
    assert request(root) == {"v": 1, "result": "unavailable"}
    print("manual pair and R requests: capture works; protected registration unavailable")


def without_service():
    run("systemctl", "stop", UNIT)
    try:
        operator("status")
        svg = BASE / "state/pairing-without-writer.svg"
        operator("pair", "--svg", str(svg))
        assert svg.is_file()
    finally:
        run("systemctl", "start", UNIT)
    print("absent writer: ordinary capture and pair remain usable")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=("setup", "verify", "without-service"))
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--writer", type=Path)
    parser.add_argument("--installer", type=Path)
    parser.add_argument("--after-reboot", action="store_true")
    args = parser.parse_args()
    if os.environ.get("LOUISELM_REQUIRE_MANAGED_REGISTRATION_SYSTEMD") != "1" or os.geteuid() != 0:
        raise RuntimeError("requires explicit disposable guest opt-in and root")
    if Path("/proc/1/comm").read_text().strip() != "systemd":
        raise RuntimeError("requires systemd PID 1")
    if args.phase == "setup":
        if not args.capture or not args.writer or not args.installer:
            parser.error("setup requires --capture, --writer and --installer")
        setup(args.capture, args.writer, args.installer)
    elif args.phase == "verify":
        verify(args.after_reboot)
    else:
        without_service()


if __name__ == "__main__":
    main()
