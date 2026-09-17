#!/usr/bin/env python3
"""Enable one existing protected canonical tracker for the installed broker.

This publishes configuration only. Prepare project permissions and a root-owned
copy of br first; this command never initializes, moves or changes tracker data.
Comment grants remain explicit, per-launch operator decisions.
"""

import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import runpy
import stat
import sys


COMMON = runpy.run_path(str(Path(__file__).with_name("install-broker-attention.py")))
CONFIG = Path("/etc/louiselm-broker-beads.json")


def check(path, directory, owners, groups=()):
    metadata = path.lstat()
    if (not (stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode))
            or (not directory and metadata.st_nlink != 1)
            or metadata.st_uid not in owners or metadata.st_mode & 0o002
            or (metadata.st_mode & 0o020 and metadata.st_gid not in groups)):
        raise ValueError("untrusted tracker path")
    for attribute in ("system.posix_acl_access", "system.posix_acl_default"):
        try:
            os.getxattr(path, attribute, follow_symlinks=False)
        except OSError as error:
            if error.errno not in (errno.ENODATA, errno.ENOTSUP):
                raise
        else:
            raise ValueError("extended tracker ACLs are unsupported")
    return metadata


def configuration(workspace, program, digest, installed):
    for path in (workspace, program):
        if not path.is_absolute() or path == Path("/") or path.resolve() != path:
            raise ValueError("paths must be canonical and absolute")
    identities = [installed[key] for key in ("operator_uid", "broker_uid", "broker_gid")]
    if any(type(value) is not int or not 0 < value < 2**32 - 1 for value in identities):
        raise ValueError("invalid installed identities")
    operator, broker, broker_group = identities
    if operator == broker:
        raise ValueError("broker must have a dedicated identity")
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise ValueError("expected exact lowercase SHA-256")
    for parent in program.parents:
        check(parent, True, (0,))
    metadata = check(program, False, (0,))
    if not metadata.st_mode & 0o111:
        raise ValueError("br must be executable")
    with program.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != digest:
            raise ValueError("br digest differs")
    for parent in (workspace, *workspace.parents):
        check(parent, True, (0, operator))
    beads = workspace / ".beads"
    if beads.lstat().st_mode & 0o007:
        raise ValueError("canonical tracker must deny other identities all access")
    remaining = 65536
    pending = [beads]
    while pending:
        entry = pending.pop()
        remaining -= 1
        if remaining < 0:
            raise ValueError("tracker tree exceeds validation bound")
        directory = stat.S_ISDIR(entry.lstat().st_mode)
        check(entry, directory, (0, operator, broker), (broker_group,))
        if directory:
            pending.extend(entry.iterdir())
    check(beads / "beads.db", False, (0, operator, broker), (broker_group,))
    return {"workspace": str(workspace), "program": str(program), "program_digest": "sha256:" + digest}


def install():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--br", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise ValueError("requires administrator")
    installed = json.loads(COMMON["read_file"](COMMON["AUTHORITY"]))
    record = configuration(args.workspace, args.br, args.sha256, installed)
    if len((json.dumps(record, sort_keys=True) + "\n").encode()) > 4096:
        raise ValueError("tracker configuration exceeds installed size bound")
    COMMON["record"](CONFIG, record)
    print(json.dumps({"configured": True, "restart_required": True,
                      "project_digest": "sha256:" + hashlib.sha256(os.fsencode(args.workspace)).hexdigest(),
                      "comment_grants": "explicit per-launch permission required"}, sort_keys=True))


if __name__ == "__main__":
    try:
        install()
    except (OSError, ValueError, KeyError):
        print("Tracker provisioning refused; inspect paths, digest, installed identities and existing configuration.", file=sys.stderr)
        sys.exit(1)
