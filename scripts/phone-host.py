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
PRIVATE_STATE = Path("var/lib/louiselm-phone-authority")
REFERENCE = Path("run/louiselm-phone-host/reference.json")
HELD_ROOT = Path("run/louiselm-phone-host/root")
HELD_OWNER_PARENT = Path("run/louiselm-phone-host/owner-parent")
HELD_CAPTURE_STAGE = Path("run/louiselm-phone-host/capture-stage")
HELD_PARENT_STAGE = Path("run/louiselm-phone-host/parent-stage")
STAGE_NAME = ".louiselm-phone-stage"
PROGRAM = Path("usr/local/lib/louiselm-phone-host/phone-host.py")
WRITER = PROGRAM.parent / "phone-writer"
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
MAX_WRITER = 512 * 1024 * 1024
HEX = re.compile(r"[0-9a-f]{64}\Z")
BOOT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\Z")
UUID = re.compile(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}\Z")
HANDLE = re.compile(r"(?:[0-9a-f]{2}){1,128}\Z")
UNIT_PATH = re.compile(r"/(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\Z")
QUERY_UNITS = ("louiselm-phone-pairing.socket", "louiselm-phone-pairing@.service")
REGISTRATION_UNITS = ("louiselm-phone-registration.socket", "louiselm-phone-registration@.service")


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


def host_mounts():
    """Read mounted filesystem/device pairs and decode kernel mountpoint escapes."""
    for line in Path("/proc/self/mountinfo").read_bytes().splitlines():
        before, after = line.split(b" - ", 1)
        fields = before.split()
        major, minor = fields[2].split(b":", 1)
        point = re.sub(rb"\\([0-7]{3})", lambda match: bytes([int(match[1], 8)]), fields[4])
        yield int(fields[0]), os.makedev(int(major), int(minor)), point, after.split()[0]


def mount_entries():
    for _, device, point, kind in host_mounts():
        yield device, point, kind


def mount_id(fd):
    for line in Path(f"/proc/self/fdinfo/{fd}").read_text(encoding="ascii").splitlines():
        if line.startswith("mnt_id:"):
            return int(line.split(":", 1)[1])
    raise ValueError("mount identity is unavailable")


def open_filesystem_stage(paths, destination):
    """Open a private stage on the destination's verified host filesystem."""
    device = os.fstat(destination).st_dev
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    for expected_id, mounted_device, point, kind in host_mounts():
        if mounted_device != device or kind != b"ext4":
            continue
        try:
            anchor = os.open(point, flags)
        except OSError:
            continue
        try:
            info = os.fstat(anchor)
            if (info.st_dev != device or info.st_uid != paths.owner or info.st_mode & 0o022
                    or mount_id(anchor) != expected_id):
                continue
            created = False
            try:
                os.mkdir(STAGE_NAME, 0o700, dir_fd=anchor)
                created = True
            except FileExistsError:
                pass
            stage = os.open(STAGE_NAME, flags, dir_fd=anchor)
            try:
                if created:
                    os.fchmod(stage, 0o700)
                    os.fsync(stage)
                    os.fsync(anchor)
                info = os.fstat(stage)
                if (info.st_dev != device or info.st_uid != paths.owner
                        or stat.S_IMODE(info.st_mode) != 0o700 or mount_id(stage) != expected_id):
                    raise ValueError("filesystem staging directory is untrusted")
                return stage
            except BaseException:
                os.close(stage)
                raise
        finally:
            os.close(anchor)
    raise ValueError("no trusted ext4 mount for the destination filesystem")


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


def validate_capture_parent(root, parent):
    try:
        child = os.open("capture", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                        dir_fd=parent)
    except OSError as error:
        raise ValueError("capture parent does not contain the registered capture directory") from error
    try:
        if not same_directory(root, child):
            raise ValueError("capture parent does not contain the registered capture directory")
    finally:
        os.close(child)


def open_registered_root(paths, raw=None):
    """Return the held directory FD; only a trusted manager launch can vouch for it."""
    return reopen_handle(catalog(read(paths, CATALOG) if raw is None else raw))


def open_registered_parent(paths, raw=None):
    value = catalog(read(paths, CATALOG) if raw is None else raw)
    if value["version"] != 3:
        raise ValueError("registered owner parent is unavailable")
    return reopen_handle(value["owner_parent"])


def open_registered_stage(paths, name, raw=None):
    if name not in ("capture_stage", "parent_stage"):
        raise ValueError("unknown staging directory")
    value = catalog(read(paths, CATALOG) if raw is None else raw)
    if value["version"] != 3:
        raise ValueError("registered staging directory is unavailable")
    fd = reopen_handle(value[name])
    info = os.fstat(fd)
    if info.st_uid != paths.owner or stat.S_IMODE(info.st_mode) != 0o700:
        os.close(fd)
        raise ValueError("registered staging directory is untrusted")
    return fd


def directory_record(fd):
    handle_type, handle = file_handle(fd)
    value = {"filesystem_uuid": filesystem_uuid(fd), "handle_type": handle_type, "handle": handle}
    reopened = reopen_handle(value)
    try:
        if not same_directory(fd, reopened):
            raise ValueError("filesystem handle reopened a different directory")
    finally:
        os.close(reopened)
    return value


def attach_root(paths, fd, held=HELD_ROOT):
    """Publish the held root to PID 1 without replacing an existing mount."""
    parent = directory(paths, held.parent, create=True)
    try:
        try:
            os.mkdir(held.name, 0o755, dir_fd=parent)
        except FileExistsError:
            pass
        target_fd = os.open(held.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=parent)
        try:
            target = os.fsencode(paths.base / held)
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
        target_fd = os.open(held.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
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


def read_at(parent, name, owner, mode, max_bytes=MAX_RECORD):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_nlink != 1
                or stat.S_IMODE(info.st_mode) != mode or info.st_size > max_bytes):
            raise ValueError("invalid provisioned file")
        with os.fdopen(fd, "rb", closefd=False) as source:
            value = source.read(max_bytes + 1)
        if len(value) > max_bytes:
            raise ValueError("oversized provisioned file")
        return value
    finally:
        os.close(fd)


def open_private_state(paths):
    parent = directory(paths, PRIVATE_STATE.parent, create=True)
    try:
        created = False
        try:
            os.mkdir(PRIVATE_STATE.name, 0o700, dir_fd=parent)
            created = True
        except FileExistsError:
            pass
        fd = os.open(PRIVATE_STATE.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                     dir_fd=parent)
        try:
            if created:
                os.fchmod(fd, 0o700)
                os.fsync(fd)
                os.fsync(parent)
            info = os.fstat(fd)
            if info.st_uid != paths.owner or stat.S_IMODE(info.st_mode) != 0o700:
                raise ValueError("private state is untrusted")
            return fd
        except BaseException:
            os.close(fd)
            raise
    finally:
        os.close(parent)


def read(paths, path, mode=0o444):
    parent = directory(paths, path.parent)
    try:
        return read_at(parent, path.name, paths.owner, mode)
    finally:
        os.close(parent)


def publish(paths, path, content, mode=0o444, *, max_bytes=MAX_RECORD):
    """Create and sync once; never replace an existing record or installation."""
    if len(content) > max_bytes:
        raise ValueError("oversized provisioned file")
    parent = directory(paths, path.parent, create=True)
    temporary = ".stage-" + secrets.token_hex(16)
    created = False
    try:
        try:
            existing = read_at(parent, path.name, paths.owner, mode, max_bytes)
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
            libc_call("renameat2", (parent, os.fsencode(temporary), parent, os.fsencode(path.name), 1),
                      (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint))
            created = False
        except FileExistsError:
            if read_at(parent, path.name, paths.owner, mode, max_bytes) != content:
                raise ValueError("existing record differs") from None
            os.unlink(temporary, dir_fd=parent)
            created = False
        except AttributeError as error:
            raise ValueError("atomic no-replace publication is unavailable") from error
        os.fsync(parent)
    finally:
        if created:
            os.unlink(temporary, dir_fd=parent)
            os.fsync(parent)
        os.close(parent)


def install_writer(paths, binary):
    """Install administrator-owned executable bytes without replacing an installation."""
    if not Path(binary).is_absolute():
        raise ValueError("writer must be an absolute path")
    resolved = Path(binary).resolve(strict=True)
    try:
        relative = resolved.relative_to(paths.base)
    except ValueError:
        raise ValueError("untrusted writer path") from None
    parent = directory(paths, relative.parent)
    try:
        fd = os.open(relative.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                     dir_fd=parent)
        try:
            info = os.fstat(fd)
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != paths.owner or info.st_nlink != 1
                    or info.st_mode & 0o022 or not info.st_mode & 0o111
                    or not 0 < info.st_size <= MAX_WRITER):
                raise ValueError("untrusted writer executable")
            with os.fdopen(fd, "rb", closefd=False) as source:
                content = source.read(info.st_size + 1)
            after = os.fstat(fd)
            if (len(content) != info.st_size or (info.st_size, info.st_mtime_ns, info.st_ctime_ns)
                    != (after.st_size, after.st_mtime_ns, after.st_ctime_ns)):
                raise ValueError("writer executable changed while reading")
        finally:
            os.close(fd)
    finally:
        os.close(parent)
    publish(paths, WRITER, content, 0o555, max_bytes=MAX_WRITER)


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
    fields = {"version": int, "filesystem_uuid": str, "handle_type": int,
              "handle": str, "receiver": str}
    initial = json.loads(raw, object_pairs_hook=object_without_duplicates)
    if isinstance(initial, dict) and initial.get("version") == 3:
        fields["owner_parent"] = dict
        fields["capture_stage"] = dict
        fields["parent_stage"] = dict
        fields["operator_uid"] = int
        fields["operator_gid"] = int
    value = decode(raw, fields)
    if (value["version"] not in (2, 3) or not valid_directory_record(value)
            or not HEX.fullmatch(value["receiver"])):
        raise ValueError("catalog is invalid")
    if value["version"] == 3:
        parent = value["owner_parent"]
        if (set(parent) != {"filesystem_uuid", "handle_type", "handle"}
                or not valid_directory_record(parent)
                or all(parent[name] == value[name] for name in parent)
                or not 0 < value["operator_uid"] < 4294967295
                or not 0 < value["operator_gid"] < 4294967295):
            raise ValueError("catalog is invalid")
        for name, destination in (("capture_stage", value), ("parent_stage", parent)):
            stage = value[name]
            if (set(stage) != {"filesystem_uuid", "handle_type", "handle"}
                    or not valid_directory_record(stage)
                    or stage["filesystem_uuid"] != destination["filesystem_uuid"]
                    or all(stage[field] == destination[field] for field in stage)):
                raise ValueError("catalog is invalid")
        if ((value["filesystem_uuid"] == parent["filesystem_uuid"])
                != (value["capture_stage"] == value["parent_stage"])):
            raise ValueError("catalog is invalid")
    return value


def valid_directory_record(value):
    return (type(value.get("filesystem_uuid")) is str
            and UUID.fullmatch(value["filesystem_uuid"]) is not None
            and value["filesystem_uuid"] != "00000000-0000-0000-0000-000000000000"
            and type(value.get("handle_type")) is int and 0 < value["handle_type"] <= 2147483647
            and type(value.get("handle")) is str and HANDLE.fullmatch(value["handle"]) is not None)


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


def registration_templates():
    here = Path(__file__).resolve().parent
    project = here.parent / "capture-service/contrib/systemd"
    return {name: (here / (name + ".in") if (here / (name + ".in")).exists()
                   else project / (name + ".in")).read_bytes() for name in REGISTRATION_UNITS}


def registration_value(paths, root, uid, gid, managed, prior=None):
    if not Path(root).is_absolute():
        raise ValueError("capture root must be absolute")
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        value = {"version": 2, **directory_record(fd)}
        if managed:
            parent_fd = os.open("..", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                                dir_fd=fd)
            try:
                validate_capture_parent(fd, parent_fd)
                parent = directory_record(parent_fd)
                capture_stage_fd = (open_registered_stage(paths, "capture_stage", prior)
                                    if prior is not None else open_filesystem_stage(paths, fd))
                try:
                    if prior is not None:
                        parent_stage_fd = open_registered_stage(paths, "parent_stage", prior)
                    elif os.fstat(fd).st_dev == os.fstat(parent_fd).st_dev:
                        parent_stage_fd = os.dup(capture_stage_fd)
                    else:
                        parent_stage_fd = open_filesystem_stage(paths, parent_fd)
                    try:
                        if (os.fstat(fd).st_dev != os.fstat(capture_stage_fd).st_dev
                                or os.fstat(parent_fd).st_dev != os.fstat(parent_stage_fd).st_dev
                                or same_directory(fd, capture_stage_fd)
                                or same_directory(parent_fd, parent_stage_fd)):
                            raise ValueError("staging directory matches a registered destination")
                        capture_stage = directory_record(capture_stage_fd)
                        parent_stage = directory_record(parent_stage_fd)
                    finally:
                        os.close(parent_stage_fd)
                finally:
                    os.close(capture_stage_fd)
                value.update({"version": 3, "owner_parent": parent, "capture_stage": capture_stage,
                              "parent_stage": parent_stage, "operator_uid": uid, "operator_gid": gid})
            finally:
                os.close(parent_fd)
        return value
    finally:
        os.close(fd)


def refuse_known_receiver(root):
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    capture = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        parent = os.open("..", flags, dir_fd=capture)
        try:
            for directory, name, entries in (
                    (capture, "tls", ("receiver-key.pem", "receiver-cert.pem", "phone-admission-barrier.json",
                                      "phone-action-owner.json", "phone-action-evidence.json")),
                    (capture, "phone-control", ("action-evidence", "action-evidence.lock")),
                    (parent, "workflow", ("phone-action-owner", "phone-action-owner.lock"))):
                try:
                    child = os.open(name, flags, dir_fd=directory)
                except FileNotFoundError:
                    continue
                except OSError as error:
                    raise ValueError("existing receiver requires recovery") from error
                try:
                    for entry in entries:
                        try:
                            os.stat(entry, dir_fd=child, follow_symlinks=False)
                        except FileNotFoundError:
                            continue
                        raise ValueError("existing receiver requires recovery")
                finally:
                    os.close(child)
        finally:
            os.close(parent)
    finally:
        os.close(capture)


def install_first(paths, root, uid, gid, binary, writer_binary):
    """Stage one private candidate; no first-origin or Send authority is implied."""
    if (type(uid) is not int or type(gid) is not int or not 0 < uid < 4294967295
            or not 0 < gid < 4294967295):
        raise ValueError("operator must be unprivileged")
    prior = None
    try:
        prior = read(paths, CATALOG)
        if catalog(prior)["version"] != 3:
            raise ValueError("existing installation requires recovery")
    except FileNotFoundError:
        pass
    except (OSError, ValueError, TypeError, UnicodeError) as error:
        raise ValueError("existing installation requires recovery") from error
    safe_executable(binary, uid, gid)
    if prior is None:
        refuse_known_receiver(root)
    setup = encode(registration_value(paths, root, uid, gid, True, prior))
    if prior is not None:
        current = catalog(prior)
        del current["receiver"]
        if encode(current) != setup:
            raise ValueError("existing installation differs and requires recovery")
    private = open_private_state(paths)
    try:
        try:
            previous = read_at(private, "setup.json", paths.owner, 0o600)
        except FileNotFoundError:
            if prior is not None or os.listdir(private):
                raise ValueError("existing private state requires recovery") from None
        else:
            if prior is None or previous != setup:
                raise ValueError("existing private state requires recovery")
            try:
                for name in ("candidate.intent", "candidate.prepared"):
                    read_at(private, name, paths.owner, 0o600)
            except (OSError, ValueError) as error:
                raise ValueError("existing private state requires recovery") from error
        install_writer(paths, writer_binary)
        publish(paths, PRIVATE_STATE / "setup.json", setup, 0o600)
        result = subprocess.run((str(paths.base / WRITER), "prepare", str(private)), check=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
                                env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"},
                                stdin=subprocess.DEVNULL, pass_fds=(private,))
        output = decode(result.stdout, {"v": int, "receiver": str})
        if output["v"] != 1 or not HEX.fullmatch(output["receiver"]):
            raise ValueError("managed writer returned an invalid candidate")
        install(paths, root, output["receiver"], uid, gid, binary,
                pin_owner_parent=True, writer_binary=writer_binary)
        return output["receiver"]
    finally:
        os.close(private)


def install(paths, root, receiver, uid, gid, binary, *, pin_owner_parent=False, writer_binary=None):
    if not HEX.fullmatch(receiver):
        raise ValueError("receiver must be a lowercase SHA-256 fingerprint")
    if (type(uid) is not int or type(gid) is not int or not 0 < uid < 4294967295
            or not 0 < gid < 4294967295):
        raise ValueError("operator must be unprivileged")
    if pin_owner_parent != (writer_binary is not None):
        raise ValueError("managed installation requires the fixed writer")
    prior = None
    if pin_owner_parent:
        try:
            prior = read(paths, CATALOG)
            if catalog(prior)["version"] != 3:
                raise ValueError("existing installation requires recovery")
        except FileNotFoundError:
            pass
        except (OSError, ValueError, TypeError, UnicodeError) as error:
            raise ValueError("existing installation requires recovery") from error
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
    if pin_owner_parent:
        writer_templates = registration_templates()
        if set(writer_templates) != set(REGISTRATION_UNITS):
            raise ValueError("registration unit templates are missing")
        for name, template in writer_templates.items():
            if name.endswith(".socket") and any(
                    marker not in template for marker in (b"@OPERATOR_UID@", b"@OPERATOR_GID@")):
                raise ValueError("registration unit template is missing a required placeholder")
            content = template
            for marker, replacement in substitutions.items():
                content = content.replace(marker, replacement)
            if re.search(rb"@[A-Z_]+@", content):
                raise ValueError("unknown registration unit placeholder")
            units[name] = content
    value = registration_value(paths, root, uid, gid, pin_owner_parent, prior)
    value["receiver"] = receiver
    record = encode(value)
    if prior is not None and prior != record:
        raise ValueError("existing installation differs and requires recovery")
    # The protected catalog is published last. Existing installations and records
    # are checked byte-for-byte and never silently replaced by a new release.
    publish(paths, PROGRAM, Path(__file__).read_bytes(), 0o444)
    publish(paths, UNIT, SERVICE, 0o444)
    for name in QUERY_UNITS:
        publish(paths, PROGRAM.parent / (name + ".in"), templates[name], 0o444)
        publish(paths, UNIT.parent / name, units[name], 0o444)
    if pin_owner_parent:
        private = open_private_state(paths)
        os.close(private)
        install_writer(paths, writer_binary)
        for name in REGISTRATION_UNITS:
            publish(paths, PROGRAM.parent / (name + ".in"), writer_templates[name], 0o444)
            publish(paths, UNIT.parent / name, units[name], 0o444)
    publish(paths, CATALOG, record)


def refresh(paths, require_pid_one=True):
    raw = read(paths, CATALOG)
    try:
        value = catalog(raw)
    except (ValueError, TypeError, UnicodeError) as error:
        raise ValueError("catalog is invalid") from error
    boot, device, inode = current(require_pid_one)
    root_fd = open_registered_root(paths, raw)
    opened = [root_fd]
    try:
        if value["version"] == 3:
            parent_fd = open_registered_parent(paths, raw)
            opened.append(parent_fd)
            validate_capture_parent(root_fd, parent_fd)
            capture_stage = open_registered_stage(paths, "capture_stage", raw)
            opened.append(capture_stage)
            parent_stage = open_registered_stage(paths, "parent_stage", raw)
            opened.append(parent_stage)
            if (os.fstat(root_fd).st_dev != os.fstat(capture_stage).st_dev
                    or os.fstat(parent_fd).st_dev != os.fstat(parent_stage).st_dev):
                raise ValueError("registered staging filesystem changed")
        attach_root(paths, root_fd)
        if value["version"] == 3:
            attach_root(paths, parent_fd, HELD_OWNER_PARENT)
            attach_root(paths, capture_stage, HELD_CAPTURE_STAGE)
            attach_root(paths, parent_stage, HELD_PARENT_STAGE)
    finally:
        for fd in reversed(opened):
            os.close(fd)
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
    first = commands.add_parser("install-first")
    first.add_argument("root", type=Path, help="absolute capture state directory")
    first.add_argument("--operator", required=True, help="unprivileged operator account")
    first.add_argument("--capture-binary", required=True, type=Path,
                       help="absolute path to the operator's capture executable")
    first.add_argument("--writer-binary", required=True, type=Path,
                       help="absolute path to the administrator-owned writer executable")
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
        if args.command == "install-first":
            install_first(paths, args.root, account.pw_uid, account.pw_gid,
                          args.capture_binary, args.writer_binary)
        else:
            install(paths, args.root, args.receiver, account.pw_uid, account.pw_gid, args.capture_binary)
        activate = [("daemon-reload",), ("enable", "--now", "louiselm-phone-host.service"),
                    ("enable", "--now", "louiselm-phone-pairing.socket")]
        if args.command == "install-first":
            activate.append(("enable", "--now", "louiselm-phone-registration.socket"))
        for command in activate:
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
