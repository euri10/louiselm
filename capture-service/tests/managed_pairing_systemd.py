#!/usr/bin/env python3
"""Exercise the real pairing query only inside an explicitly disposable systemd VM.

The initial --setup --manual-assets mode installs the current host provisioner,
then renders the shipped consumer units for a same-boot test. It does not prove
persistent root binding or production installer integration. --verify can run
after a guest reboot when the provisioner supports that binding.
"""

import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import sys
import time


BASE = Path("/opt/louiselm-managed-pairing-fixture")
HOST_UNIT = Path("/etc/systemd/system/louiselm-phone-host.service")
SOCKET = Path("/etc/systemd/system/louiselm-phone-pairing.socket")
SERVICE = Path("/etc/systemd/system/louiselm-phone-pairing@.service")
RUNTIME = Path("/run/louiselm-phone-host/pairing.sock")


def run(*args, env=None, check=True):
    result = subprocess.run(args, env=env, text=True, capture_output=True, timeout=30, check=False)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} failed ({result.returncode}): {result.stderr[-2000:]}")
    return result


def as_operator(*args, state=None, check=True):
    root = state or BASE / "state"
    return run("runuser", "-u", "vm", "--", "env", f"LOUISELM_CAPTURE_STATE_DIR={root}",
               f"LOUISELM_CAPTURE_DATA_DIR={BASE / 'data'}", f"LOUISELM_CAPTURE_CONFIG_DIR={BASE / 'config'}",
               str(BASE / "louiselm-capture"), *args, check=check)


def setup(capture, installer, templates, manual_assets):
    for path in (BASE, HOST_UNIT, SOCKET, SERVICE, Path("/var/lib/louiselm-phone-host"),
                 Path("/usr/local/lib/louiselm-phone-host"), Path("/run/louiselm-phone-host")):
        if path.exists() or path.is_symlink():
            raise RuntimeError(f"fixture refuses existing path: {path}")
    account = pwd.getpwnam("vm")
    BASE.mkdir(mode=0o755)
    binary = BASE / "louiselm-capture"
    shutil.copyfile(capture, binary)
    binary.chmod(0o755)
    for name in ("state", "data", "config", "other"):
        path = BASE / name
        path.mkdir()
        os.chown(path, account.pw_uid, account.pw_gid)
    audio = BASE / "audio.wav"
    audio.write_bytes(b"disposable fixture audio")
    audio.chmod(0o644)
    as_operator("ingest-local", "--file", str(audio), "--recorded-at-ms", "1", "--duration-ms", "1", "--mime", "audio/wav")
    as_operator("status")  # The existing command creates an ordinary pairing registry.
    registry = BASE / "state/louiselm/capture/pairing/pairing.json"
    data = json.loads(registry.read_text())
    data["devices"] = [{"device_id": "fixture-phone", "device_name": "fixture phone", "paired_at_ms": 2,
                        "last_delivery_at_ms": None, "credential_sha256": "0" * 64}]
    registry.write_text(json.dumps(data))
    os.chown(registry, account.pw_uid, account.pw_gid)
    os.symlink(BASE / "state", BASE / "state-alias")
    other_root = BASE / "other/louiselm/capture"
    other_root.mkdir(parents=True)
    os.chown(BASE / "other/louiselm", account.pw_uid, account.pw_gid)
    os.chown(other_root, account.pw_uid, account.pw_gid)
    root = BASE / "state/louiselm/capture"
    installer_args = [sys.executable, "-I", str(installer), "install", str(root), "a" * 64]
    if not manual_assets:
        installer_args.extend(("--operator", "vm", "--capture-binary", str(binary)))
    run(*installer_args)
    if manual_assets:
        replacements = {"@OPERATOR_UID@": str(account.pw_uid), "@OPERATOR_GID@": str(account.pw_gid),
                        "@CAPTURE_BINARY@": str(binary), "@CAPTURE_ROOT@": str(root)}
        for template, destination in (("louiselm-phone-pairing.socket.in", SOCKET),
                                      ("louiselm-phone-pairing@.service.in", SERVICE)):
            text = (templates / template).read_text()
            for old, new in replacements.items():
                text = text.replace(old, new)
            destination.write_text(text)
            destination.chmod(0o644)
        run("systemctl", "daemon-reload")
        run("systemctl", "enable", "--now", SOCKET.name)
    if not SOCKET.exists() or not SERVICE.exists():
        raise RuntimeError("provisioner did not install the managed pairing units")
    run("loginctl", "enable-linger", "vm")
    deadline = time.monotonic() + 20
    while not Path(f"/run/user/{account.pw_uid}/systemd/private").exists():
        if time.monotonic() >= deadline:
            raise RuntimeError("fixture user manager did not start")
        time.sleep(0.1)
    (BASE / "initial-boot-id").write_text(Path("/proc/sys/kernel/random/boot_id").read_text())
    print("setup complete; consumer units " + ("rendered manually for same-boot test" if manual_assets else "installed by provisioner"))


def verify(after_reboot):
    if after_reboot:
        last = BASE / "last-verified-boot-id"
        previous = (last if last.exists() else BASE / "initial-boot-id").read_text().strip()
        current = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        assert previous != current, "expected a different guest boot ID"
    normal = json.loads(as_operator("pairing-status").stdout)
    assert [device["device_name"] for device in normal["devices"]] == ["fixture phone"]
    alias = json.loads(as_operator("pairing-status", state=BASE / "state-alias").stdout)
    assert alias == normal
    wrong = as_operator("pairing-status", state=BASE / "other", check=False)
    assert wrong.returncode != 0 and "root mismatch" in wrong.stderr, wrong.stderr
    account = pwd.getpwnam("vm")
    output = BASE / "state/user-result.json"
    units = Path(account.pw_dir) / ".config/systemd/user"
    units.mkdir(parents=True, exist_ok=True)
    for directory in (units, units.parent, units.parent.parent):
        os.chown(directory, account.pw_uid, account.pw_gid)
    unit = units / "louiselm-pairing-fixture.service"
    unit.write_text(f"[Service]\nType=oneshot\nEnvironment=LOUISELM_CAPTURE_STATE_DIR={BASE / 'state'}\n"
                    f"ExecStart={BASE / 'louiselm-capture'} pairing-status\nStandardOutput=file:{output}\n")
    os.chown(unit, account.pw_uid, account.pw_gid)
    manager = ("runuser", "-u", "vm", "--", "env", f"XDG_RUNTIME_DIR=/run/user/{account.pw_uid}",
               f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{account.pw_uid}/systemd/private", "systemctl", "--user")
    run(*manager, "daemon-reload")
    run(*manager, "start", unit.name)
    assert json.loads(output.read_text()) == normal
    captures = json.loads(as_operator("list").stdout)
    assert len(captures) == 1 and captures[0]["record"]["duration_ms"] == 1
    print("manual and user-unit queries: registered root and alias matched, other root refused, capture list healthy")
    if after_reboot:
        last.write_text(current + "\n")
        print(f"guest reboot verified: {previous} -> {current}")


def without_service():
    run("systemctl", "stop", SOCKET.name)
    result = as_operator("pairing-status", check=False)
    assert result.returncode != 0 and "managed pairing service unavailable" in result.stderr, result.stderr
    assert len(json.loads(as_operator("list").stdout)) == 1
    json.loads(as_operator("status").stdout)
    run("systemctl", "start", SOCKET.name)
    print("absent socket: pairing query unavailable; capture list and status remain healthy")


def move_root():
    root = BASE / "state/louiselm/capture"
    moved = root.with_name("capture-moved")
    if moved.exists() or root.is_symlink():
        raise RuntimeError("fixture root was already moved")
    root.rename(moved)
    root.symlink_to(moved.name)
    print("registered fixture root moved; its former path is an alias")


def without_host():
    catalog = Path("/var/lib/louiselm-phone-host/catalog.json")
    held = catalog.with_name("catalog.gate-hold")
    if held.exists() or not catalog.is_file():
        raise RuntimeError("fixture catalog is unavailable or already held")
    run("systemctl", "stop", HOST_UNIT.name)
    catalog.rename(held)
    try:
        result = as_operator("pairing-status", check=False)
        assert result.returncode != 0 and not result.stdout, result.stdout
        host = run("systemctl", "is-failed", HOST_UNIT.name, check=False)
        assert host.stdout.strip() == "failed", host.stdout
        assert len(json.loads(as_operator("list").stdout)) == 1
        json.loads(as_operator("status").stdout)
    finally:
        held.rename(catalog)
        run("systemctl", "reset-failed", HOST_UNIT.name)
        run("systemctl", "start", HOST_UNIT.name)
    print("missing fixture catalog: query unavailable; capture list and status remain healthy")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=("setup", "verify", "without-service", "move-root", "without-host"))
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--installer", type=Path)
    parser.add_argument("--templates", type=Path)
    parser.add_argument("--manual-assets", action="store_true")
    parser.add_argument("--after-reboot", action="store_true")
    args = parser.parse_args()
    if os.environ.get("LOUISELM_REQUIRE_MANAGED_PAIRING_SYSTEMD") != "1" or os.geteuid() != 0:
        raise RuntimeError("requires explicit disposable guest opt-in and root")
    if Path("/proc/1/comm").read_text().strip() != "systemd":
        raise RuntimeError("requires systemd PID 1")
    if args.phase == "setup":
        if not args.capture or not args.installer or not args.templates:
            parser.error("setup requires --capture, --installer and --templates")
        setup(args.capture, args.installer, args.templates, args.manual_assets)
    elif args.phase == "verify":
        verify(args.after_reboot)
    elif args.phase == "without-service":
        without_service()
    elif args.phase == "move-root":
        move_root()
    else:
        without_host()


if __name__ == "__main__":
    main()
