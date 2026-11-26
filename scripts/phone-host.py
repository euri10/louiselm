#!/usr/bin/env python3
"""Provision candidate phone host records; they never grant Send authority."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import sys


CATALOG = Path("var/lib/louiselm-phone-host/catalog.json")
REFERENCE = Path("run/louiselm-phone-host/reference.json")
PROGRAM = Path("usr/local/lib/louiselm-phone-host/phone-host.py")
UNIT = Path("etc/systemd/system/louiselm-phone-host.service")
SERVICE = b"""[Unit]
Description=Refresh the LouiseLM phone host reference
After=local-fs.target
Before=multi-user.target

[Service]
Type=oneshot
ExecStart=/usr/bin/python3 -I /usr/local/lib/louiselm-phone-host/phone-host.py refresh
RemainAfterExit=yes
RuntimeDirectory=louiselm-phone-host
RuntimeDirectoryMode=0755
RuntimeDirectoryPreserve=yes
NoNewPrivileges=yes
PrivateNetwork=no
PrivateUsers=no
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/run/louiselm-phone-host
CapabilityBoundingSet=CAP_SYS_PTRACE

[Install]
WantedBy=multi-user.target
"""
MAX_RECORD = 65536
HEX = re.compile(r"[0-9a-f]{64}\Z")
BOOT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\Z")


class Paths:
    """Filesystem anchor; production uses / and root, fixtures use a private tree."""

    def __init__(self, base, owner):
        self.base = Path(base)
        self.owner = owner


def directory(paths, relative, create=False):
    """Open each trusted directory without following symlinks."""
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = os.open(paths.base, flags)
    try:
        for name in ("", *relative.parts):
            if name:
                if create:
                    try:
                        os.mkdir(name, 0o755, dir_fd=fd)
                    except FileExistsError:
                        pass
                    else:
                        new = os.open(name, flags, dir_fd=fd)
                        try:
                            os.fchmod(new, 0o755)
                            os.fsync(new)
                        finally:
                            os.close(new)
                        os.fsync(fd)
                new = os.open(name, flags, dir_fd=fd)
                os.close(fd)
                fd = new
            info = os.fstat(fd)
            if info.st_uid != paths.owner or info.st_mode & 0o022:
                raise ValueError("untrusted provisioning directory")
        return fd
    except BaseException:
        os.close(fd)
        raise


def read_at(parent, name, owner, mode):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_nlink != 1
                or stat.S_IMODE(info.st_mode) != mode or info.st_size > MAX_RECORD):
            raise ValueError("invalid provisioned file")
        with os.fdopen(fd, "rb", closefd=False) as source:
            value = source.read(MAX_RECORD + 1)
        if len(value) > MAX_RECORD:
            raise ValueError("oversized provisioned file")
        return value
    finally:
        os.close(fd)


def read(paths, path, mode=0o444):
    parent = directory(paths, path.parent)
    try:
        return read_at(parent, path.name, paths.owner, mode)
    finally:
        os.close(parent)


def publish(paths, path, content, mode=0o444):
    """Create and sync once; never replace an existing record or installation."""
    parent = directory(paths, path.parent, create=True)
    temporary = ".stage-" + secrets.token_hex(16)
    created = False
    try:
        try:
            existing = read_at(parent, path.name, paths.owner, mode)
        except FileNotFoundError:
            pass
        else:
            if existing != content:
                raise ValueError("existing record differs")
            os.fsync(parent)
            return
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                     0o600, dir_fd=parent)
        created = True
        with os.fdopen(fd, "wb") as output:
            output.write(content)
            output.flush()
            os.fchmod(output.fileno(), mode)
            os.fsync(output.fileno())
        try:
            os.link(temporary, path.name, src_dir_fd=parent, dst_dir_fd=parent, follow_symlinks=False)
        except FileExistsError:
            if read_at(parent, path.name, paths.owner, mode) != content:
                raise ValueError("existing record differs") from None
        os.unlink(temporary, dir_fd=parent)
        created = False
        os.fsync(parent)
    finally:
        if created:
            os.unlink(temporary, dir_fd=parent)
            os.fsync(parent)
        os.close(parent)


def encode(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def object_without_duplicates(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate record field")
        result[key] = value
    return result


def decode(raw, fields):
    value = json.loads(raw, object_pairs_hook=object_without_duplicates)
    if not isinstance(value, dict) or set(value) != set(fields):
        raise ValueError("invalid record fields")
    for key, kind in fields.items():
        if type(value[key]) is not kind:
            raise ValueError("invalid record type")
    return value


def root_identity(root):
    if not Path(root).is_absolute():
        raise ValueError("capture root must be absolute")
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        info = os.fstat(fd)
        return info.st_dev, info.st_ino
    finally:
        os.close(fd)


def catalog(raw):
    value = decode(raw, {"version": int, "device": int, "inode": int, "receiver": str})
    if (value["version"] != 1 or value["device"] < 0 or value["inode"] <= 0
            or not HEX.fullmatch(value["receiver"])):
        raise ValueError("catalog is invalid")
    return value


def current(require_pid_one):
    boot = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
    if not BOOT.fullmatch(boot):
        raise ValueError("boot identity is invalid")
    network = os.stat("/proc/self/ns/net")
    if require_pid_one:
        pid_one = os.stat("/proc/1/ns/net")
        if (network.st_dev, network.st_ino) != (pid_one.st_dev, pid_one.st_ino):
            raise ValueError("refresh is outside the PID 1 network namespace")
    return boot, network.st_dev, network.st_ino


def install(paths, root, receiver):
    if not HEX.fullmatch(receiver):
        raise ValueError("receiver must be a lowercase SHA-256 fingerprint")
    device, inode = root_identity(root)
    record = encode({"version": 1, "device": device, "inode": inode, "receiver": receiver})
    # The protected catalog is published last. Existing installations and records
    # are checked byte-for-byte and never silently replaced by a new release.
    publish(paths, PROGRAM, Path(__file__).read_bytes(), 0o444)
    publish(paths, UNIT, SERVICE, 0o444)
    publish(paths, CATALOG, record)


def refresh(paths, require_pid_one=True):
    raw = read(paths, CATALOG)
    try:
        catalog(raw)
    except (ValueError, TypeError, UnicodeError) as error:
        raise ValueError("catalog is invalid") from error
    boot, device, inode = current(require_pid_one)
    record = {"version": 1, "boot_id": boot, "net_device": device, "net_inode": inode,
              "catalog_sha256": hashlib.sha256(raw).hexdigest()}
    publish(paths, REFERENCE, encode(record))


def inspect(paths, root, receiver):
    """Return a refusal code, including when every candidate record matches."""
    try:
        raw = read(paths, CATALOG)
    except FileNotFoundError:
        return "catalog_missing"
    except (OSError, ValueError):
        return "catalog_invalid"
    try:
        selected = catalog(raw)
    except (ValueError, TypeError, UnicodeError):
        return "catalog_invalid"
    try:
        ref_raw = read(paths, REFERENCE)
    except FileNotFoundError:
        return "reference_missing"
    except (OSError, ValueError):
        return "reference_invalid"
    try:
        ref = decode(ref_raw, {"version": int, "boot_id": str, "net_device": int,
                               "net_inode": int, "catalog_sha256": str})
        if (ref["version"] != 1 or not BOOT.fullmatch(ref["boot_id"])
                or ref["net_device"] < 0 or ref["net_inode"] <= 0
                or not HEX.fullmatch(ref["catalog_sha256"])
                or ref["catalog_sha256"] != hashlib.sha256(raw).hexdigest()):
            return "reference_invalid"
        boot, device, inode = current(False)
        if ref["boot_id"] != boot:
            return "boot_mismatch"
        if (ref["net_device"], ref["net_inode"]) != (device, inode):
            return "namespace_mismatch"
        if selected["receiver"] != receiver:
            return "receiver_mismatch"
        if (selected["device"], selected["inode"]) != root_identity(root):
            return "root_mismatch"
    except (OSError, ValueError, TypeError, UnicodeError):
        return "reference_invalid"
    # Root-owned-looking paths and namespace IDs alone do not authenticate the
    # host mount view or rule out old/foreign registrars and shared storage.
    return "scope_unverified"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("install", "check"):
        command = commands.add_parser(name)
        command.add_argument("root", type=Path, help="absolute capture state directory")
        command.add_argument("receiver", help="receiver SPKI SHA-256 fingerprint")
    commands.add_parser("refresh")
    args = parser.parse_args()
    paths = Paths("/", 0)
    if args.command == "check":
        reason = inspect(paths, args.root, args.receiver)
        print(json.dumps({"version": 1, "status": "unavailable", "reason": reason}))
        return 2
    if os.geteuid() != 0:
        raise ValueError("install and refresh require an administrator")
    if args.command == "refresh":
        refresh(paths)
    else:
        install(paths, args.root, args.receiver)
        for command in (("daemon-reload",), ("enable", "--now", "louiselm-phone-host.service")):
            subprocess.run(("/usr/bin/systemctl", *command), check=True,
                           env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"}, stdin=subprocess.DEVNULL)
        print("Phone host records installed; Send scope remains unavailable.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"phone host provisioning refused: {error}", file=sys.stderr)
        sys.exit(1)
