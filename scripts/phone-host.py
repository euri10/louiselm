#!/usr/bin/env python3
"""Provision candidate phone host records; they never grant Send authority."""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import secrets
import stat
import subprocess
import sys


CATALOG = Path("var/lib/louiselm-phone-host/catalog.json")
REFERENCE = Path("run/louiselm-phone-host/reference.json")
HELD_ROOT = Path("run/louiselm-phone-host/root")
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
PrivateMounts=no
CapabilityBoundingSet=CAP_SYS_PTRACE CAP_DAC_READ_SEARCH CAP_SYS_ADMIN

[Install]
WantedBy=multi-user.target
"""
MAX_RECORD = 65536
HEX = re.compile(r"[0-9a-f]{64}\Z")
BOOT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\Z")
UUID = re.compile(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}\Z")
HANDLE = re.compile(r"(?:[0-9a-f]{2}){1,128}\Z")
UNIT_PATH = re.compile(r"/(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\Z")
QUERY_UNITS = ("louiselm-phone-pairing.socket", "louiselm-phone-pairing@.service")


class FileHandle(ctypes.Structure):
    _fields_ = [("size", ctypes.c_uint), ("type", ctypes.c_int),
                ("bytes", ctypes.c_ubyte * 128)]


def libc_call(name, arguments, argtypes):
    libc = ctypes.CDLL(None, use_errno=True)
    function = getattr(libc, name)
    function.argtypes = argtypes
    function.restype = ctypes.c_int
    result = function(*arguments)
    if result < 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))
    return result


def file_handle(fd):
    handle = FileHandle()
    handle.size = len(handle.bytes)
    mount_id = ctypes.c_int()
    libc_call("name_to_handle_at", (fd, b"", ctypes.byref(handle), ctypes.byref(mount_id), 0x1000),
              (ctypes.c_int, ctypes.c_char_p, ctypes.POINTER(FileHandle),
               ctypes.POINTER(ctypes.c_int), ctypes.c_int))
    if not 0 < handle.size <= len(handle.bytes) or handle.type <= 0:
        raise ValueError("unsupported filesystem handle")
    return handle.type, bytes(handle.bytes[:handle.size]).hex()


def mount_entries():
    """Read mounted filesystem/device pairs and decode kernel mountpoint escapes."""
    for line in Path("/proc/self/mountinfo").read_bytes().splitlines():
        before, after = line.split(b" - ", 1)
        fields = before.split()
        major, minor = fields[2].split(b":", 1)
        point = re.sub(rb"\\([0-7]{3})", lambda match: bytes([int(match[1], 8)]), fields[4])
        yield os.makedev(int(major), int(minor)), point, after.split()[0]


def probe_device(device):
    path = f"/dev/block/{os.major(device)}:{os.minor(device)}"
    try:
        info = os.stat(path)
    except FileNotFoundError:
        raise ValueError("filesystem is not backed by an available block device") from None
    if not stat.S_ISBLK(info.st_mode) or info.st_rdev != device:
        raise ValueError("filesystem device is unavailable")
    result = subprocess.run(("/usr/sbin/blkid", "-p", "-s", "UUID", "-s", "TYPE", "-o", "export", path),
                            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"}, stdin=subprocess.DEVNULL)
    fields = {}
    for line in result.stdout.decode("ascii").splitlines():
        key, value = line.split("=", 1)
        if key in fields:
            raise ValueError("ambiguous filesystem identity")
        fields[key] = value
    if (fields.get("TYPE") != "ext4" or not UUID.fullmatch(fields.get("UUID", ""))
            or fields["UUID"] == "00000000-0000-0000-0000-000000000000"):
        raise ValueError("only block-backed ext4 filesystems with a UUID are supported")
    return fields["UUID"]


def uuid_device(uuid):
    info = os.stat("/dev/disk/by-uuid/" + uuid)
    if not stat.S_ISBLK(info.st_mode):
        raise ValueError("filesystem UUID does not identify a block device")
    return info.st_rdev


def filesystem_device(uuid):
    """Resolve a UUID independently and reject duplicate mounted ext4 filesystems."""
    device = uuid_device(uuid)
    if probe_device(device) != uuid:
        raise ValueError("filesystem UUID does not match its block device")
    others = {dev for dev, _, kind in mount_entries() if kind == b"ext4" and dev != device}
    for other in others:
        if probe_device(other) == uuid:
            raise ValueError("filesystem UUID is ambiguous")
    return device


def filesystem_uuid(fd):
    device = os.fstat(fd).st_dev
    uuid = probe_device(device)
    if filesystem_device(uuid) != device:
        raise ValueError("filesystem UUID identifies another device")
    return uuid


def reopen_handle(record):
    device = filesystem_device(record["filesystem_uuid"])
    handle = FileHandle()
    data = bytes.fromhex(record["handle"])
    handle.size, handle.type = len(data), record["handle_type"]
    handle.bytes[:len(data)] = data
    for mounted_device, point, kind in mount_entries():
        if mounted_device != device or kind != b"ext4":
            continue
        try:
            anchor = os.open(point, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        except OSError:
            continue
        try:
            if os.fstat(anchor).st_dev != device:
                continue
            fd = libc_call("open_by_handle_at", (anchor, ctypes.byref(handle),
                           os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC),
                           (ctypes.c_int, ctypes.POINTER(FileHandle), ctypes.c_int))
            if os.fstat(fd).st_dev != device or os.fstat(fd).st_nlink == 0:
                os.close(fd)
                raise ValueError("registered directory is unavailable")
            return fd
        finally:
            os.close(anchor)
    raise ValueError("registered filesystem is not mounted")


def same_directory(first, second):
    a, b = os.fstat(first), os.fstat(second)
    return (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)


def open_registered_root(paths, raw=None):
    """Return the held directory FD; only a trusted manager launch can vouch for it."""
    return reopen_handle(catalog(read(paths, CATALOG) if raw is None else raw))


def attach_root(paths, fd):
    """Publish the held root to PID 1 without replacing an existing mount."""
    parent = directory(paths, HELD_ROOT.parent, create=True)
    try:
        try:
            os.mkdir(HELD_ROOT.name, 0o755, dir_fd=parent)
        except FileExistsError:
            pass
        target_fd = os.open(HELD_ROOT.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=parent)
        try:
            target = os.fsencode(paths.base / HELD_ROOT)
            mounted = any(point == target for _, point, _ in mount_entries())
            if mounted:
                if not same_directory(fd, target_fd):
                    raise ValueError("existing root mount differs")
                return
            info = os.fstat(target_fd)
            if info.st_uid != paths.owner or info.st_mode & 0o022 or os.listdir(target_fd):
                raise ValueError("root mountpoint is untrusted")
            libc_call("mount", (os.fsencode(f"/proc/self/fd/{fd}"), target, None, 4096, None),
                      (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_void_p))
        finally:
            os.close(target_fd)
        target_fd = os.open(HELD_ROOT.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=parent)
        try:
            if not same_directory(fd, target_fd):
                raise ValueError("published root mount differs")
        finally:
            os.close(target_fd)
    finally:
        os.close(parent)


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


def catalog(raw):
    value = decode(raw, {"version": int, "filesystem_uuid": str, "handle_type": int,
                         "handle": str, "receiver": str})
    if (value["version"] != 2 or not UUID.fullmatch(value["filesystem_uuid"])
            or value["filesystem_uuid"] == "00000000-0000-0000-0000-000000000000"
            or not 0 < value["handle_type"] <= 2147483647 or not HANDLE.fullmatch(value["handle"])
            or not HEX.fullmatch(value["receiver"])):
        raise ValueError("catalog is invalid")
    return value


def current(require_pid_one):
    boot = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
    if not BOOT.fullmatch(boot):
        raise ValueError("boot identity is invalid")
    network = os.stat("/proc/self/ns/net")
    if require_pid_one:
        for name in ("net", "mnt"):
            local = os.stat("/proc/self/ns/" + name)
            pid_one = os.stat("/proc/1/ns/" + name)
            if (local.st_dev, local.st_ino) != (pid_one.st_dev, pid_one.st_ino):
                raise ValueError("provisioning is outside the PID 1 " + name + " namespace")
    return boot, network.st_dev, network.st_ino


def safe_executable(binary, uid, gid):
    if not Path(binary).is_absolute():
        raise ValueError("capture binary must be an absolute path")
    resolved = Path(binary).resolve(strict=True)
    if not UNIT_PATH.fullmatch(str(resolved)):
        raise ValueError("capture binary path cannot be represented safely in a systemd unit")
    for component in (resolved, *resolved.parents):
        info = component.stat()
        if info.st_uid not in (0, uid) or info.st_mode & 0o022:
            raise ValueError("capture binary has an untrusted owner or writable component")
        if component == resolved:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError("capture binary is not a regular file")
        elif not stat.S_ISDIR(info.st_mode):
            raise ValueError("capture binary parent is not a directory")
    subprocess.run(("/usr/bin/setpriv", "--reuid", str(uid), "--regid", str(gid), "--init-groups",
                    "/usr/bin/test", "-x", str(resolved)), check=True, timeout=5,
                   env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"}, stdin=subprocess.DEVNULL)
    return str(resolved)


def query_templates():
    here = Path(__file__).resolve().parent
    project = here.parent / "capture-service/contrib/systemd"
    templates = {}
    for name in QUERY_UNITS:
        local = here / (name + ".in")
        templates[name] = (local if local.exists() else project / (name + ".in")).read_bytes()
    return templates


def install(paths, root, receiver, uid, gid, binary):
    if not HEX.fullmatch(receiver):
        raise ValueError("receiver must be a lowercase SHA-256 fingerprint")
    if uid <= 0 or gid <= 0:
        raise ValueError("operator must be unprivileged")
    executable = safe_executable(binary, uid, gid)
    templates = query_templates()
    substitutions = {b"@OPERATOR_UID@": str(uid).encode(), b"@OPERATOR_GID@": str(gid).encode(),
                     b"@CAPTURE_BINARY@": os.fsencode(executable),
                     b"@CAPTURE_ROOT@": os.fsencode(Path("/") / HELD_ROOT)}
    units = {}
    for name, template in templates.items():
        required = (b"@OPERATOR_UID@", b"@OPERATOR_GID@")
        if name == "louiselm-phone-pairing@.service":
            required += (b"@CAPTURE_BINARY@", b"@CAPTURE_ROOT@")
        if any(marker not in template for marker in required):
            raise ValueError("query unit template is missing a required placeholder")
        content = template
        for marker, replacement in substitutions.items():
            content = content.replace(marker, replacement)
        if re.search(rb"@[A-Z_]+@", content):
            raise ValueError("unknown query unit placeholder")
        units[name] = content
    if set(units) != set(QUERY_UNITS):
        raise ValueError("query unit templates are missing")
    if not Path(root).is_absolute():
        raise ValueError("capture root must be absolute")
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        uuid = filesystem_uuid(fd)
        handle_type, handle = file_handle(fd)
        value = {"version": 2, "filesystem_uuid": uuid, "handle_type": handle_type,
                 "handle": handle, "receiver": receiver}
        reopened = reopen_handle(value)
        try:
            if not same_directory(fd, reopened):
                raise ValueError("filesystem handle reopened a different directory")
        finally:
            os.close(reopened)
    finally:
        os.close(fd)
    record = encode(value)
    # The protected catalog is published last. Existing installations and records
    # are checked byte-for-byte and never silently replaced by a new release.
    publish(paths, PROGRAM, Path(__file__).read_bytes(), 0o444)
    publish(paths, UNIT, SERVICE, 0o444)
    for name in QUERY_UNITS:
        publish(paths, PROGRAM.parent / (name + ".in"), templates[name], 0o444)
        publish(paths, UNIT.parent / name, units[name], 0o444)
    publish(paths, CATALOG, record)


def refresh(paths, require_pid_one=True):
    raw = read(paths, CATALOG)
    try:
        catalog(raw)
    except (ValueError, TypeError, UnicodeError) as error:
        raise ValueError("catalog is invalid") from error
    boot, device, inode = current(require_pid_one)
    root_fd = open_registered_root(paths, raw)
    try:
        attach_root(paths, root_fd)
    finally:
        os.close(root_fd)
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
    except (OSError, ValueError, TypeError, UnicodeError):
        return "reference_invalid"
    if not Path(root).is_absolute():
        return "root_mismatch"
    try:
        root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            if (os.fstat(root_fd).st_dev != uuid_device(selected["filesystem_uuid"])
                    or file_handle(root_fd) != (selected["handle_type"], selected["handle"])):
                return "root_mismatch"
        finally:
            os.close(root_fd)
    except (OSError, ValueError):
        return "root_unavailable"
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
        if name == "install":
            command.add_argument("--operator", required=True, help="unprivileged operator account")
            command.add_argument("--capture-binary", required=True, type=Path,
                                 help="absolute path to the operator's capture executable")
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
        current(True)
        try:
            account = pwd.getpwnam(args.operator)
        except KeyError:
            raise ValueError("operator account does not exist") from None
        install(paths, args.root, args.receiver, account.pw_uid, account.pw_gid, args.capture_binary)
        for command in (("daemon-reload",), ("enable", "--now", "louiselm-phone-host.service"),
                        ("enable", "--now", "louiselm-phone-pairing.socket")):
            subprocess.run(("/usr/bin/systemctl", *command), check=True,
                           env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"}, stdin=subprocess.DEVNULL)
        print("Phone host records installed; Send scope remains unavailable.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"phone host provisioning refused: {error}", file=sys.stderr)
        sys.exit(1)
