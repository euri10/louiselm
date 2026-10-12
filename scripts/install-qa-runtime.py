#!/usr/bin/env python3
"""Explicit administrator publication of one reviewed fixed QA-runtime proposal."""
import argparse
import copy
import fcntl
import hashlib
import json
import os
import re
import stat
import sys
import tempfile
from pathlib import Path

REGISTRY = Path("/var/lib/louiselm/registry")
RUNTIMES = Path("/var/lib/louiselm/qa-runtimes")
EXECUTABLE = "louiselm-tool-test-agent"
INVENTORY = "louiselm-discovery.json"
EMPTY_REGISTRY = {"schema": "louiselm.launch.registry/1", "entries": []}


def require(condition):
    if not condition:
        raise ValueError("QA runtime publication refused")


def read_file(path, limit):
    require(path.is_absolute())
    directory = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in path.parts[1:-1]:
            require(part not in (".", ".."))
            next_directory = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = next_directory
        descriptor = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    finally:
        os.close(directory)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size <= limit)
        data = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        require(len(data) == before.st_size and before.st_mtime_ns == after.st_mtime_ns
                and before.st_ctime_ns == after.st_ctime_ns and before.st_mode == after.st_mode
                and after.st_nlink == 1)
        return data


def sha(data):
    return hashlib.sha256(data).hexdigest()


def check_directory(path, allowed):
    for target in (path, *path.parents):
        metadata = target.lstat()
        require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid in allowed and not metadata.st_mode & 0o022)


def measured_proposal(source, reviewed, operator):
    require(source.is_absolute() and source.resolve() == source)
    check_directory(source, (0, operator))
    check_directory(source / "runtime", (0, operator))
    data = read_file(source / "proposal.json", 65536)
    require("sha256:" + sha(data) == reviewed)
    value = json.loads(data)
    require(set(value) == {"schema", "state", "release_id", "source_commit", "agent", "runtime", "envelope", "inventory_digest"}
            and value["schema"] == "louiselm.qa.runtime-proposal/1" and value["state"] == "proposed"
            and re.fullmatch(r"sha256:[0-9a-f]{64}", value["release_id"]) is not None
            and isinstance(value["source_commit"], str) and 0 < len(value["source_commit"]) <= 128)
    identity = "qa-" + value["release_id"][7:31]
    root = RUNTIMES / value["release_id"][7:]
    agent, runtime, envelope = value["agent"], value["runtime"], value["envelope"]
    require(agent == {"id": identity, "provider": "offline-test", "runtime_id": identity,
                      "arguments": [], "environment": {}, "tool_integration": "louiselm.test-tool-integration/1"})
    require(set(runtime) == {"id", "root", "executable", "executable_sha256", "adapters", "version", "origin"}
            and runtime["id"] == identity and runtime["root"] == str(root) and runtime["executable"] == EXECUTABLE
            and runtime["origin"] == "release:" + value["release_id"]
            and re.fullmatch(r"[0-9a-f]{64}", runtime["executable_sha256"]) is not None
            and isinstance(runtime["version"], str) and bool(runtime["version"]))
    require(envelope == {"id": "desktop-qa-denied", "network": "denied",
                         "description": "Explicit operator-only offline desktop conformance QA"})
    executable = read_file(source / "runtime" / EXECUTABLE, 128 * 1024 * 1024)
    inventory = read_file(source / "runtime" / INVENTORY, 65536)
    require(sha(executable) == runtime["executable_sha256"]
            and runtime["adapters"] == [{"path": INVENTORY, "sha256": sha(inventory)}]
            and value["inventory_digest"] == "sha256:" + sha(inventory))
    parsed = json.loads(inventory)
    require(parsed["runtime_id"] == identity and parsed["executable_sha256"] == sha(executable))
    return value, root, executable, inventory


def make_directory(path):
    # Explicit modes: sudo passes the caller's umask through (louiselm-by3oy).
    path.mkdir(mode=0o700)
    os.chmod(path, 0o755)


def write_new(path, data, mode):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fchmod(stream.fileno(), mode)
        os.fsync(stream.fileno())


def read_registry(path):
    # The launcher reads a missing registry file as empty (skills-core/src/registry.rs read_list).
    try:
        return read_file(path, 4 * 1024 * 1024)
    except FileNotFoundError:
        return None


def registry_document(original, entry):
    document = copy.deepcopy(EMPTY_REGISTRY) if original is None else json.loads(original)
    require(set(document) == {"schema", "entries"} and document["schema"] == "louiselm.launch.registry/1"
            and isinstance(document["entries"], list) and len(document["entries"]) < 4096
            and all(isinstance(item, dict) and isinstance(item.get("id"), str) for item in document["entries"]))
    existing = [item for item in document["entries"] if item.get("id") == entry["id"]]
    require(not existing or existing == [entry])
    return document, existing


def append(path, entry, original):
    document, existing = registry_document(original, entry)
    if existing:
        return
    document["entries"].append(entry)
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".qa-registry-", delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(json.dumps(document, separators=(",", ":")).encode())
            stream.flush()
            os.fsync(stream.fileno())
            os.chmod(temporary, 0o644)
            if original is None:
                os.link(temporary, path)  # exclusive: a concurrently created file refuses
            else:
                require(read_file(path, 4 * 1024 * 1024) == original)
                os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def publish(source, reviewed, operator):
    value, root, executable, inventory = measured_proposal(source, reviewed, operator)
    if not REGISTRY.exists():
        check_directory(REGISTRY.parent, (0,))
        make_directory(REGISTRY)
    check_directory(REGISTRY, (0,))
    # Separate privileged review is mandatory; there is no automatic caller from the QA coordinator.
    descriptor = os.open(REGISTRY / ".qa-runtime.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "rb") as lock:
        metadata = os.fstat(lock.fileno())
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_nlink == 1
                and stat.S_IMODE(metadata.st_mode) == 0o600)
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        originals = {name: read_registry(REGISTRY / name)
                     for name in ("runtimes.json", "envelopes.json", "agents.json")}
        # Validate duplicates before any runtime publication, preserving every old entry.
        for name, key in (("runtimes.json", "runtime"), ("envelopes.json", "envelope"), ("agents.json", "agent")):
            if originals[name] is not None:
                metadata = (REGISTRY / name).lstat()
                require(metadata.st_uid == 0 and not metadata.st_mode & 0o022)
            registry_document(originals[name], value[key])
        if not RUNTIMES.exists():
            check_directory(RUNTIMES.parent, (0,))
            make_directory(RUNTIMES)
        check_directory(RUNTIMES, (0,))
        if root.exists():
            check_directory(root, (0,))
            require(read_file(root / EXECUTABLE, 128 * 1024 * 1024) == executable
                    and read_file(root / INVENTORY, 65536) == inventory)
        else:
            make_directory(root)
            make_directory(root / "builtin")
            make_directory(root / "builtin/skills")
            write_new(root / EXECUTABLE, executable, 0o555)
            write_new(root / INVENTORY, inventory, 0o444)
        # Publish Agent last: intermediate files cannot make the new Agent runnable.
        for name, key in (("runtimes.json", "runtime"), ("envelopes.json", "envelope"), ("agents.json", "agent")):
            append(REGISTRY / name, value[key], originals[name])
        for directory in (root, RUNTIMES, REGISTRY):
            descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
    return {"agent_id": value["agent"]["id"], "runtime_id": value["runtime"]["id"],
            "envelope_id": value["envelope"]["id"], "release_id": value["release_id"],
            "state": "registered_not_authorized"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("proposal", type=Path)
    parser.add_argument("--reviewed", required=True)
    args = parser.parse_args()
    try:
        operator = os.environ.get("SUDO_UID", "")
        require(os.geteuid() == 0 and operator.isdecimal() and str(int(operator)) == operator and int(operator) > 0)
        authority = json.loads(read_file(Path("/usr/local/lib/louiselm/launcher/public-config.json"), 65536))
        require(authority["operator_uid"] == int(operator))
        print(json.dumps(publish(args.proposal, args.reviewed, int(operator))))
        return 0
    except (ValueError, KeyError, TypeError, OSError):
        print("QA runtime publication refused; retain proposal and any partial QA-only registration. No launch or signature.", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
