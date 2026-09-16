#!/usr/bin/env python3
"""Explicitly share existing public Admission evidence with the installed broker.

Run as root with --store ABSOLUTE_PATH. Never creates/promotes a skills store,
shares signing keys, replaces trust, or changes existing Admission bytes.
"""

import argparse
import json
import os
from pathlib import Path
import runpy
import stat
import sys


COMMON = runpy.run_path(str(Path(__file__).with_name("install-broker-attention.py")))
CONFIG = Path("/etc/louiselm-broker-admission.json")


def open_evidence(entry, directory, operator_uid):
    """Never let a mutable operator pathname redirect root metadata changes."""
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for index, part in enumerate(entry.parts[1:]):
            final = index == len(entry.parts) - 2
            flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
            if not final or directory:
                flags |= os.O_DIRECTORY
            child = os.open(part, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
            metadata = os.fstat(descriptor)
            if metadata.st_uid not in ((operator_uid,) if final else (0, operator_uid)):
                raise ValueError("evidence owner changed")
        metadata = os.fstat(descriptor)
        if not directory and (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1):
            raise ValueError("evidence type changed")
        result, descriptor = descriptor, None
        return result
    finally:
        if descriptor is not None:
            os.close(descriptor)


def plan(store, operator_uid):
    if not store.is_absolute() or store.resolve() != store or store == Path("/"):
        raise ValueError("store must be a canonical absolute path")
    for parent in store.parents:
        metadata = parent.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid not in (0, operator_uid)
                or metadata.st_mode & 0o022):
            raise ValueError("untrusted store ancestor")
    targets = [(store, True), (store / "provenance.json", False)]
    for name in ("packages", "staging"):
        directory = store / name
        targets.append((directory, True))
        for parent, directories, files in os.walk(directory, followlinks=False):
            targets.extend((Path(parent) / name, True) for name in directories)
            if name == "packages":
                targets.extend((Path(parent) / name, False) for name in files)
    targets.extend((store / "trust" / name, False) for name in ("roles.json", "roles.lock"))
    targets.append((store / "trust", True))
    generations = store / "generations"
    if generations.exists():
        targets.append((generations, True))
        targets.extend((entry, False) for entry in generations.iterdir())
    if len(targets) > 65536:
        raise ValueError("evidence bound exceeded")
    for entry, directory in targets:
        metadata = entry.lstat()
        if (metadata.st_uid != operator_uid
                or (directory and not stat.S_ISDIR(metadata.st_mode))
                or (not directory and (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1))):
            raise ValueError("unexpected evidence inode")
    return targets


def install():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--store", type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise ValueError("requires administrator")
    installed = json.loads(COMMON["read_file"](COMMON["AUTHORITY"]))
    operator_uid, broker_uid, broker_gid = [installed[key] for key in ("operator_uid", "broker_uid", "broker_gid")]
    if (any(type(value) is not int or not 0 < value < 2**32 - 1 for value in (operator_uid, broker_uid, broker_gid))
            or operator_uid == broker_uid):
        raise ValueError("invalid installed identities")
    targets = plan(args.store, operator_uid)
    provenance = json.loads((args.store / "provenance.json").read_bytes())
    if provenance.get("trusted") is not True or not provenance.get("created_by_release"):
        raise ValueError("requires an existing trusted store")
    trust = json.loads((args.store / "trust/roles.json").read_bytes())
    if not isinstance(trust.get("trust_domain"), str) or not 0 < len(trust["trust_domain"]) <= 256:
        raise ValueError("invalid enrolled trust domain")
    record = {"store": str(args.store), "trust_domain": trust["trust_domain"],
              "operator_uid": operator_uid, "broker_uid": broker_uid}
    if CONFIG.exists() and json.loads(COMMON["read_file"](CONFIG)) != record:
        raise ValueError("existing source differs; preserve and inspect it")
    # Exclusive lock also excludes active read-only verification while permissions change.
    import fcntl
    with os.fdopen(open_evidence(args.store / "trust/roles.lock", False, operator_uid), "rb") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for entry, directory in targets:
            descriptor = open_evidence(entry, directory, operator_uid)
            try:
                metadata = os.fstat(descriptor)
                os.fchown(descriptor, operator_uid, broker_gid)
                # Preserve executable bits and immutability of packaged files.
                mode = 0o2750 if directory else ((metadata.st_mode & 0o700) | 0o040)
                if directory and entry.is_relative_to(args.store / "staging"):
                    mode = 0o2700  # Inherit reader GID without exposing incomplete candidates.
                os.fchmod(descriptor, mode)
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        generations = args.store / "generations"
        if not generations.exists():
            descriptor = open_evidence(args.store, True, operator_uid)
            try:
                os.mkdir("generations", mode=0o2750, dir_fd=descriptor)
                child = os.open("generations", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
                try:
                    os.fchown(child, operator_uid, broker_gid)
                    os.fchmod(child, 0o2750)
                    os.fsync(child)
                    os.fsync(descriptor)
                finally:
                    os.close(child)
            finally:
                os.close(descriptor)
        # Protected configuration is the enable switch, published only after read access.
        COMMON["record"](CONFIG, record)
    print("Broker Admission evidence enabled read-only; signing remains in the skills tool.")


if __name__ == "__main__":
    try:
        install()
    except (OSError, ValueError, KeyError):
        print("Admission provisioning refused; inspect store ownership, trust and installed identities.", file=sys.stderr)
        sys.exit(1)
