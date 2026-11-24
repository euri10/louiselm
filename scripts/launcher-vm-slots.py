#!/usr/bin/env python3
"""Coordinate credential-free VM slots; the shell wrapper owns QEMU and SSH."""

from contextlib import contextmanager
from datetime import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import stat
import subprocess
import sys
import time


GIB = 1024**3
DISK_LIMIT = 32 * GIB
SLOT_OVERHEAD = 16 * 1024**2  # qcow2 metadata, firmware, seed and small logs
LAUNCHER = Path(__file__).with_name("launcher-vm")


def fail(message):
    raise ValueError(message)


def private(directory, create=False):
    if create:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.resolve() != directory or not directory.is_dir():
        fail(f"directory must be absolute without symlink/dot components: {directory}")
    metadata = directory.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        fail(f"directory must be private (0700) and owned by you: {directory}")


@contextmanager
def locked(path):
    with path.open("a") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            fail(f"another slot command is running; retry after it finishes ({path})")
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def identity(server, acp_id):
    expression = 'luaeval(' + json.dumps(
        '(function() local id, err = require("louiselm.session").identity(_A) return id or err end)()'
    ) + ', ' + json.dumps(acp_id) + ')'
    try:
        result = subprocess.run(
            ["nvim", "--headless", "--server", server, "--remote-expr", expression],
            capture_output=True, text=True, timeout=10, check=False,
        )
    except subprocess.TimeoutExpired:
        return ""
    return result.stdout.strip() if result.returncode == 0 else ""


def verified_owner():
    requested = os.environ.get("LOUISELM_VM_OWNER", "")
    acp_id = requested.partition("/")[2] if requested else os.environ.get("CODEX_SESSION_ID", "")
    server = os.environ.get("NVIM", "")
    if not acp_id or not server:
        fail("slot ownership needs NVIM and your exposed CODEX_SESSION_ID or LOUISELM_VM_OWNER=agent/ACP-id")
    owner = identity(server, acp_id)
    if not re.fullmatch(r"[^/\s]+/" + re.escape(acp_id), owner) or (requested and owner != requested):
        fail("session.identity() did not verify your Session; no slot action taken")
    return owner, server


def owner_state(record):
    answer = identity(record["server"], record["owner"].partition("/")[2])
    if answer == record["owner"]:
        return "live"
    if answer == "no live Session has ACP session id " + record["owner"].partition("/")[2]:
        return "gone"
    # An unreachable editor is not evidence of death. Reuse the project's
    # transcript liveness policy; missing/ambiguous evidence stays protected.
    if answer:
        return "unknown"
    result = subprocess.run(
        [str(LAUNCHER.with_name("agent-liveness-snapshot"))],
        input=record["owner"] + "\n", capture_output=True, text=True, timeout=30, check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        rows = [json.loads(line) for line in result.stdout.splitlines()]
        if len(rows) == 1 and rows[0]["holder"] == record["owner"]:
            expires = datetime.fromisoformat(rows[0]["expires_ts"].replace("Z", "+00:00")).timestamp()
            return "live" if expires > time.time() else "gone"
    return "unknown"


def allocated(path):
    return path.stat().st_blocks * 512 if path.exists() else 0


class Pool:
    def __init__(self, root, base_name):
        self.root = root
        self.base = root / "bases" / base_name
        self.slots = root / "slots"
        self.limit = self.setting("LOUISELM_VM_MAX_SLOTS", 2, 1, 8)
        self.floor = self.setting("LOUISELM_VM_MIN_FREE_GIB", 20, 1, 1024) * GIB

    @staticmethod
    def setting(name, default, lower, upper):
        value = os.environ.get(name, str(default))
        if not value.isdecimal() or not lower <= int(value) <= upper:
            fail(f"{name} must be an integer from {lower} to {upper}")
        return int(value)

    def layout(self, cache):
        state = cache / "louiselm-launcher-vm"
        if not state.is_absolute() or state.resolve() != state or any(c in str(state) for c in ",\n\r"):
            fail("cache path must be absolute without symlink/dot components, comma or newline")
        digest = hashlib.sha256(str(state).encode()).hexdigest()
        return {"id": digest[:12], "state": str(state),
                "unit": f"louiselm-launcher-vm-{digest[:12]}.service",
                "port": 23000 + int(digest[:8], 16) % 30000, "base": str(self.base)}

    def records(self):
        records = []
        if self.slots.exists():
            private(self.root)
            private(self.slots)
            for path in sorted(self.slots.glob("*.json")):
                record = json.loads(path.read_text())
                layout = self.layout(Path(record["state"]).parent)
                if (record.keys() != layout.keys() | {"owner", "server", "created"}
                        or path.stem != layout["id"]
                        or any(record[k] != layout[k] for k in ("id", "state", "unit", "port"))
                        or not re.fullmatch(r"[^/\s]+/[^/\s]+", record["owner"])
                        or not isinstance(record["server"], str)
                        or not isinstance(record["created"], int)
                        or Path(record["base"]).parent != self.base.parent
                        or not re.fullmatch(r"prepared-[0-9a-f]{16}\.qcow2", Path(record["base"]).name)):
                    fail(f"invalid slot record; no action taken: {path}")
                private(Path(record["state"]))
                marker = json.loads((Path(record["state"]) / ".slot.json").read_text())
                if marker != {"pool": str(self.root), "id": record["id"]}:
                    fail(f"slot marker mismatch; no action taken: {path}")
                records.append(record)
        return records

    def runtime(self, record, action, args=(), capture=False):
        return subprocess.run(
            ["bash", str(LAUNCHER), "--slot-runtime", record["state"], str(record["port"]),
             record["base"], action, *args],
            capture_output=capture, text=True, check=True,
        )

    def admission(self, records, destination, extra=0):
        device = destination.stat().st_dev
        reserved = sum(max(0, DISK_LIMIT - allocated(Path(r["state"]) / "run.qcow2")) + SLOT_OVERHEAD
                       for r in records if Path(r["state"]).stat().st_dev == device)
        free = shutil.disk_usage(destination).free
        needed = self.floor + reserved + extra
        if free < needed:
            fail(f"disk admission refused: {free / GIB:.1f} GiB free, {needed / GIB:.1f} GiB needed "
                 f"({self.floor / GIB:g} GiB floor plus growth reservations). "
                 "Use --slot status; --slot cleanup for your slot or --slot cleanup --gone. "
                 "Keep shared bases and retained/first-Run evidence.")

    def publish_base(self, source):
        source = source.resolve(strict=True)
        if source.name != self.base.name:
            fail(f"existing base must match current inputs: {self.base.name}")
        chain = json.loads(subprocess.run(
            ["qemu-img", "info", "--backing-chain", "--output=json", str(source)],
            capture_output=True, text=True, check=True,
        ).stdout)
        if chain[0]["virtual-size"] != DISK_LIMIT:
            fail("prepared base must have the expected 32 GiB virtual disk")
        paths = [Path(image["filename"]) for image in chain] + [Path(str(source) + ".vars.fd")]
        for path in paths:
            metadata = path.stat()
            if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                    or metadata.st_mode & 0o222):
                fail(f"base and complete backing chain must already be owned and read-only: {path}")
        # Reference the existing chain in place. No copy, rebase, rename or chmod
        # of legacy guests, including bases used by a running default slot.
        firmware = Path(str(self.base) + ".vars.fd")
        if firmware.is_symlink():
            if firmware.resolve() != Path(str(source) + ".vars.fd"):
                fail("incomplete shared base publication points at different firmware; inspect it first")
        else:
            firmware.symlink_to(str(source) + ".vars.fd")
        # Publish the disk last; interrupted publication can be retried without
        # advertising a base whose firmware link is not present yet.
        self.base.symlink_to(source)

    def prepare(self, cache, source=None):
        owner, server = verified_owner()
        layout = self.layout(cache)
        private(self.root, create=True)
        private(self.slots, create=True)
        private(self.base.parent, create=True)
        with locked(self.root / "admission.lock"):
            records = self.records()
            existing = next((r for r in records if r["id"] == layout["id"]), None)
            if existing:
                if existing["owner"] != owner:
                    fail(f"slot owned by {existing['owner']}; choose another XDG_CACHE_HOME")
                if existing["base"] != str(self.base):
                    fail("slot has older inputs; cleanup your slot before preparing again")
                self.admission(records, Path(existing["state"]))
                return existing
            if len(records) >= self.limit:
                fail(f"slot cap reached ({self.limit}); use --slot status and cleanup, or wait")
            if any(r["port"] == layout["port"] for r in records):
                fail("slot SSH port collision; choose another XDG_CACHE_HOME")
            private(cache, create=True)
            state = Path(layout["state"])
            marker = {"pool": str(self.root), "id": layout["id"]}
            if state.exists():
                private(state)
                if (not (state / ".slot.json").is_file()
                        or json.loads((state / ".slot.json").read_text()) != marker):
                    fail("existing VM directory is not a throwaway slot; leave retained/default/first-Run VMs alone")
                if (state / "run.qcow2").exists():
                    fail("unregistered overlay exists; inspect it before reuse")
            self.admission(records, cache, DISK_LIMIT + SLOT_OVERHEAD)
            if not self.base.exists():
                if source:
                    self.publish_base(Path(source))
                else:
                    fail("no shared base for current inputs; use --slot prepare --base EXISTING_BASE. "
                         "Prepare missing inputs once with the default launcher workflow when its unit is free; "
                         "slot preparation never provisions or copies another base.")
            private(state, create=True)
            (state / ".slot.json").write_text(json.dumps(marker) + "\n")
            record = {**layout, "owner": owner, "server": server, "created": int(time.time())}
            pending = self.slots / (layout["id"] + ".pending")
            pending.write_text(json.dumps(record) + "\n")
            pending.replace(self.slots / (layout["id"] + ".json"))
            return record

    def selected(self, cache):
        slot_id = self.layout(cache)["id"]
        record = next((r for r in self.records() if r["id"] == slot_id), None)
        if record is None:
            fail("slot is not prepared; run --slot prepare [--base EXISTING_BASE]")
        return record

    def owned(self, record):
        owner, _ = verified_owner()
        if record["owner"] != owner:
            fail(f"slot owned by {record['owner']}; no action taken")

    def start(self, cache):
        with locked(self.root / "admission.lock"):
            record = self.selected(cache)
            self.owned(record)
            if record["base"] != str(self.base):
                fail("slot has older inputs; cleanup your slot before preparing again")
            with locked(Path(record["state"]) / "slot.lock"):
                records = self.records()
                if len(records) > self.limit:
                    fail(f"slot cap exceeded ({self.limit}); cleanup a slot before starting")
                self.admission(records, Path(record["state"]))
                with socket.socket() as probe:
                    probe.bind(("127.0.0.1", record["port"]))
                self.runtime(record, "start")

    def cleanup(self, cache, gone=False):
        with locked(self.root / "admission.lock"):
            records = self.records() if gone else [self.selected(cache)]
            for record in records:
                if gone:
                    if owner_state(record) != "gone":
                        continue
                else:
                    self.owned(record)
                state = Path(record["state"])
                with locked(state / "slot.lock"):
                    # Recheck death under both locks, immediately before stop.
                    if gone and owner_state(record) != "gone":
                        continue
                    self.runtime(record, "stop")
                    for name in ("run.qcow2", "run.qcow2.vars.fd", "seed.iso", "client-key",
                                 "client-key.pub", "host-key", "host-key.pub", "known_hosts"):
                        (state / name).unlink(missing_ok=True)
                    for name in ("meta-data", "user-data"):
                        (state / "seed" / name).unlink(missing_ok=True)
                    if (state / "seed").exists():
                        (state / "seed").rmdir()
                    (self.slots / (record["id"] + ".json")).unlink()
                    print(f"launcher-vm: removed throwaway overlay for {record['owner']} at {state}; base/logs kept")

    def status(self):
        records = self.records()
        for record in records:
            record["owner_state"] = owner_state(record)
            record["age_seconds"] = max(0, int(time.time()) - record["created"])
            record["disk_bytes"] = sum(p.lstat().st_blocks * 512 for p in Path(record["state"]).rglob("*")
                                       if not p.is_symlink())
            result = subprocess.run(["systemctl", "--user", "show", record["unit"],
                                     "-p", "ActiveState", "--value"],
                                    capture_output=True, text=True, check=True)
            record["active_state"] = result.stdout.strip()
        bases = [{"path": str(path), "disk_bytes": allocated(path)}
                 for path in sorted(self.base.parent.glob("prepared-*.qcow2"))]
        return {"slots": records, "shared_bases": bases, "max_slots": self.limit, "floor_bytes": self.floor}


def main(args):
    os.umask(0o077)
    if os.getuid() == 0:
        fail("never run this host wrapper as root")
    if not args:
        fail("--slot needs plan, prepare, start, status, cleanup, stop, logs, exec, put or get")
    action, *rest = args
    valid = ((action in ("plan", "start", "status", "stop", "logs") and not rest)
             or (action == "prepare" and (not rest or (len(rest) == 2 and rest[0] == "--base")))
             or (action == "cleanup" and rest in ([], ["--gone"]))
             or (action == "exec" and bool(rest)) or (action in ("put", "get") and len(rest) == 2))
    if not valid:
        fail("invalid throwaway slot command; USB, Provider egress and retained reset are unavailable")
    template = json.loads(subprocess.run(["bash", str(LAUNCHER), "plan"],
                                        capture_output=True, text=True, check=True).stdout)
    pool = Pool(Path.home() / ".cache/louiselm-launcher-vm-pool", Path(template["base_image"]).name)
    cache = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache")))
    if action == "plan":
        layout = pool.layout(cache)
        plan = json.loads(pool.runtime(layout, "plan", capture=True).stdout)
        plan.update(ssh_port=layout["port"], max_slots=pool.limit, floor_bytes=pool.floor,
                    overlay_growth_bytes=DISK_LIMIT)
        print(json.dumps(plan, indent=2))
    elif action == "prepare":
        print(json.dumps(pool.prepare(cache, rest[1] if rest else None), indent=2))
    elif action == "status":
        print(json.dumps(pool.status(), indent=2))
    elif action == "start":
        pool.start(cache)
    elif action == "cleanup":
        pool.cleanup(cache, gone=bool(rest))
    else:
        record = pool.selected(cache)
        pool.owned(record)
        with locked(Path(record["state"]) / "slot.lock"):
            pool.runtime(record, action, rest)


def cli():
    try:
        main(sys.argv[1:])
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        # CalledProcessError includes the entire guest command, which can carry
        # sensitive data even in a credential-free test. Preserve only status.
        message = (f"slot command failed (exit {error.returncode}); inspect preceding diagnostics or --slot logs"
                   if isinstance(error, subprocess.CalledProcessError) else str(error))
        print(f"launcher-vm: {message}", file=sys.stderr)
        sys.exit(error.returncode if isinstance(error, subprocess.CalledProcessError) else 1)


if __name__ == "__main__":
    cli()
