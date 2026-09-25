#!/usr/bin/env python3
"""Inspect the embedded object; opt in to the existing privileged KVM proofs."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PROBES = ROOT / "scripts/probes/codex-kernel-guard"
PROGRAMS = {"endpoint_send", "invalidate_exec", "protect_runtime", "invalidate_listener", "owner_exit", "owner_exec"}
MAPS = {"tasks", "connections", "policy", "ports", "listeners", "owners", "lost", "upstreams"}


def inspect(path):
    symbols = subprocess.check_output(["readelf", "--wide", "--symbols", path], text=True, timeout=10)
    rows = [line.split() for line in symbols.splitlines()]
    functions = {row[-1] for row in rows if len(row) == 8 and row[3] == "FUNC"}
    objects = {row[-1] for row in rows if len(row) == 8 and row[3:5] == ["OBJECT", "GLOBAL"]}
    if functions != PROGRAMS or objects != MAPS | {"LICENSE"}:
        raise ValueError(f"unexpected guard inventory: programs={sorted(functions)}, objects={sorted(objects)}")
    sections = subprocess.check_output(["readelf", "--wide", "--sections", path], text=True, timeout=10)
    if ".BTF " not in sections or ".BTF.ext " not in sections:
        raise ValueError("guard lacks BTF relocation data")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disposable-vm", action="store_true")
    parser.add_argument("launcher", type=Path)
    args = parser.parse_args()
    if args.disposable_vm:
        machine = subprocess.check_output(["systemd-detect-virt", "--vm"], text=True, timeout=5).strip()
        if machine != "kvm":
            raise ValueError("privileged Sender guard checks require a disposable KVM guest")
    result = subprocess.run([args.launcher.resolve(), "__sender-guard-object"], capture_output=True, timeout=10, check=True)
    data = result.stdout
    if result.stderr or data[:6] != b"\x7fELF\x02\x01" or data[18:20] != bytes([247, 0]):
        raise ValueError("launcher did not emit a clean little-endian BPF ELF object")
    with tempfile.TemporaryDirectory(prefix="louiselm-sender-guard-", dir="/var/tmp") as directory:
        path = Path(directory) / "sender-guard.bpf.o"
        path.write_bytes(data)
        inspect(path)
        print(f"embedded Sender guard: {len(data)} bytes, sha256={hashlib.sha256(data).hexdigest()}", flush=True)
        if not args.disposable_vm:
            return
        # The synthetic runtime has a distinct UID and cannot traverse the
        # guest operator's home. Only these public fixtures become readable.
        Path(directory).chmod(0o755)
        for name in ("ownership_probe.py", "binding_probe.py", "lifetime_probe.py", "guard_probe.py"):
            fixture = Path(directory) / name
            shutil.copyfile(PROBES / name, fixture)
            fixture.chmod(0o644)
        # The ownership fixture's missing-hook control loads this binding-only
        # sibling. It is compiled from the same reviewed source, never downloaded.
        subprocess.run([
            "clang", "-target", "bpfel", "-D__TARGET_ARCH_x86", "-g", "-O2", "-Wall", "-Werror",
            "-I", "/usr/include/x86_64-linux-gnu", "-c", PROBES / "binding.bpf.c",
            "-o", path.with_name("binding.bpf.o"),
        ], check=True, timeout=30)
        variants = [(), ("--before-check",), ("--supervisor-exec", "--orderly-close"), ("--broker-crash",)]
        for variant in variants:
            print(f"Sender guard ownership: {variant or ('owner-death-at-write',)}", flush=True)
            output = subprocess.check_output([
                "sudo", "-n", "env", "PYTHONDONTWRITEBYTECODE=1",
                "timeout", "--kill-after=10s", "90s", sys.executable,
                Path(directory) / "ownership_probe.py", "--disposable-vm", path, *variant,
            ], text=True, timeout=105)
            report = json.loads(output)
            if report.get("verdict") != "OWNERSHIP_COMPONENT_PASS_NOT_VERIFIED":
                raise ValueError("ownership proof did not complete")
            print(json.dumps(report), flush=True)
        print("embedded Sender guard VM gate: passed (component evidence only)")


if __name__ == "__main__":
    main()
