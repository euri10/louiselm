#!/usr/bin/env python3
"""Provision broker-only Attention access from installed launcher identities.

Run as root after installing the dedicated broker and capture-service. Existing
credentials/configuration are verified, never replaced or silently re-keyed.
"""

import hashlib
import json
import os
from pathlib import Path
import pwd
import secrets
import stat
import subprocess
import sys


AUTHORITY = Path("/usr/local/lib/louiselm/launcher/public-config.json")
STATE = Path("/var/lib/louiselm-attention")
TOKEN = STATE / "producer-capability"
RUNTIME = Path("/run/louiselm-attention")
RECEIVER = Path("/etc/louiselm-capture-broker.json")
SENDER = Path("/etc/louiselm-broker-attention.json")
TMPFILES = Path("/etc/tmpfiles.d/louiselm-broker-attention.conf")


def secure_parents(path):
    for parent in path.parents:
        metadata = parent.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o022:
            raise ValueError("provisioning path has an untrusted parent")


def read_file(path, uid=0, gid=0, mode=None):
    secure_parents(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        metadata = os.fstat(source.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1
                or metadata.st_uid != uid or metadata.st_gid != gid
                or metadata.st_mode & 0o022 or metadata.st_size > 16384
                or (mode is not None and stat.S_IMODE(metadata.st_mode) != mode)):
            raise ValueError("provisioned file has unexpected ownership, type, size or permissions")
        content = source.read(16385)
        if len(content) > 16384:
            raise ValueError("provisioned file is oversized")
        return content


def directory(path, uid, gid, mode):
    secure_parents(path)
    try:
        path.mkdir(mode=mode)
    except FileExistsError:
        pass
    else:
        os.chown(path, uid, gid)
        path.chmod(mode)
    metadata = path.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != uid
            or metadata.st_gid != gid or stat.S_IMODE(metadata.st_mode) != mode):
        raise ValueError("provisioned directory differs; preserve it and inspect its identity")


def publish(path, content, uid=0, gid=0, mode=0o644):
    secure_parents(path)
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    except FileExistsError:
        if read_file(path, uid, gid, mode) != content:
            raise ValueError("existing provisioned file differs; preserve it and inspect configuration")
        return
    with os.fdopen(fd, "wb") as output:
        os.fchown(output.fileno(), uid, gid)
        os.fchmod(output.fileno(), mode)
        output.write(content)
        output.flush()
        os.fsync(output.fileno())
    parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(parent)
    finally:
        os.close(parent)


def record(path, value):
    publish(path, (json.dumps(value, sort_keys=True) + "\n").encode())


def install():
    if os.geteuid() != 0 or len(sys.argv) != 1:
        raise ValueError("run as root with no arguments after launcher installation")
    config = json.loads(read_file(AUTHORITY))
    identities = [config.get(key) for key in ("broker_uid", "broker_gid", "operator_uid", "operator_gid")]
    if any(type(value) is not int or value <= 0 or value >= 2**32 - 1 for value in identities):
        raise ValueError("installed identities must be positive numeric UID/GID values")
    broker_uid, broker_gid, receiver_uid, receiver_gid = identities
    if broker_uid == receiver_uid or broker_gid == receiver_gid:
        raise ValueError("broker and capture operator must have separate identities")
    if pwd.getpwuid(broker_uid).pw_gid != broker_gid or pwd.getpwuid(receiver_uid).pw_gid != receiver_gid:
        raise ValueError("installed identities do not match the account database")
    directory(STATE, 0, 0, 0o711)
    try:
        token = read_file(TOKEN, broker_uid, broker_gid, 0o400)
    except FileNotFoundError:
        token = secrets.token_hex(32).encode()
    if len(token) != 64 or any(byte not in b"0123456789abcdef" for byte in token):
        raise ValueError("existing producer credential is invalid; preserve it for inspection")
    publish(TOKEN, token, broker_uid, broker_gid, 0o400)
    record(SENDER, {"socket": str(RUNTIME / "project.sock"),
                    "capability_file": str(TOKEN), "receiver_uid": receiver_uid})
    publish(TMPFILES, f"d {RUNTIME} 0711 {receiver_uid} {receiver_gid} -\n".encode())
    # Validate before tmpfiles, which would otherwise repair an unexpected owner.
    directory(RUNTIME, receiver_uid, receiver_gid, 0o711)
    subprocess.run(["systemd-tmpfiles", "--create", str(TMPFILES)], check=True)
    # Publish the receiver policy last: its presence enables the new listener.
    record(RECEIVER, {"socket": str(RUNTIME / "project.sock"), "broker_uid": broker_uid,
                      "capability_sha256": hashlib.sha256(token).hexdigest()})
    print("Broker Attention provisioned; restart the updated capture-service user unit.")


if __name__ == "__main__":
    try:
        install()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError):
        # Never include configuration contents or credential bytes in diagnostics.
        print("Broker Attention provisioning refused; inspect installed identities, ownership and existing files.", file=sys.stderr)
        sys.exit(1)
