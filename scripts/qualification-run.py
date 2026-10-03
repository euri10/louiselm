#!/usr/bin/env python3
"""Explicit operator-only paired trial; preparation never executes fixture code."""
import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("qualification_preview", ROOT / "scripts/qualification-preview.py")
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)


def read_selection(path):
    metadata = path.lstat()
    preview.require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid()
                    and stat.S_IMODE(metadata.st_mode) == 0o600 and metadata.st_size <= 65536,
                    "selection must be operator-owned mode 0600, regular and at most 64 KiB")
    value = preview.read_manifest(path)
    preview.fields(value, {"schema", "manifest", "policy_revision", "workload", "retention_days",
                           "envelope", "input_manifest", "cache", "snapshot_parent", "input_group"}, "selection")
    preview.require(value["schema"] == "louiselm.operator.qualification-run/1", "unsupported selection schema")
    preview.require(value["retention_days"] == 7, "explicit seven-day installed workspace retention required")
    preview.fields(value["workload"], {"kind", "id"}, "workload")
    preview.require(value["workload"]["kind"] == "main", "only direct main comparisons are supported")
    preview.nonempty(value["workload"]["id"], "workload.id")
    preview.nonempty(value["policy_revision"], "policy_revision")
    for key in ("manifest", "cache", "snapshot_parent"):
        preview.require(isinstance(value[key], str) and Path(value[key]).is_absolute(), f"{key} must be absolute")
    preview.require(type(value["input_group"]) is int and 0 < value["input_group"] < 2**32, "input_group must be a trusted sharing GID")
    preview.require(isinstance(value["envelope"], dict) and isinstance(value["input_manifest"], dict), "resolved envelope and input_manifest required")
    return value


def selected_inputs(manifest, base):
    """Recheck every selected byte immediately before freezing the owned copy."""
    contents, prompts, total = {}, [], 0
    for fixture in manifest["fixtures"]:
        items = [fixture["input"]] + fixture["snapshots"]["instructions"] + fixture["snapshots"]["source"]
        texts = {}
        for item in items:
            name, size, text = preview.selected_file(base, item, fixture["id"], manifest["limits"]["input_bytes"] - total)
            preview.require(all(32 <= ord(char) < 127 for char in name) and ".git" not in name.split("/"), "unsupported source snapshot path")
            total += size
            data = text.encode("utf-8")
            preview.require(name not in contents or contents[name] == data, "contradictory selected fixture bytes")
            contents[name], texts[name] = data, text
        prompts.append("\n\n".join(texts[item["path"]] for item in fixture["snapshots"]["instructions"])
                       + "\n\n" + texts[fixture["input"]["path"]])
    return contents, prompts


def git(arguments, checkout, data=None):
    # No developer Git config, hooks, signing, filters or shell interpretation.
    env = {"PATH": os.defpath, "HOME": str(checkout), "GIT_CONFIG_NOSYSTEM": "1",
           "GIT_CONFIG_GLOBAL": os.devnull, "GIT_AUTHOR_NAME": "Qualification fixture",
           "GIT_AUTHOR_EMAIL": "fixture@invalid", "GIT_COMMITTER_NAME": "Qualification fixture",
           "GIT_COMMITTER_EMAIL": "fixture@invalid", "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z",
           "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z"}
    result = subprocess.run(["/usr/bin/git", *arguments], cwd=checkout, env=env, input=data,
                            capture_output=True, timeout=10, check=False)
    preview.require(result.returncode == 0, "private fixture Git plumbing failed")
    return result.stdout.strip().decode("ascii")


def fixture_repository(contents, checkout):
    checkout.mkdir(mode=0o700)
    git(["init", "--quiet", "--initial-branch=qualification"], checkout)
    for name, data in sorted(contents.items()):
        destination = checkout / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        destination.chmod(0o600)
        blob = git(["hash-object", "-w", "--no-filters", "--stdin"], checkout, data)
        git(["update-index", "--add", "--cacheinfo", "100644", blob, name], checkout)
    tree = git(["write-tree"], checkout)
    commit = git(["commit-tree", tree], checkout, b"Selected qualification baseline\n")
    git(["update-ref", "refs/heads/qualification", commit], checkout)
    return commit


def verification_plan(manifest):
    commands = [argv for fixture in manifest["fixtures"] for argv in fixture["acceptance"]["commands"]]
    preview.require(0 < len(commands) <= 32, "trial requires 1..32 declared machine checks")
    timeout = min(300000, manifest["limits"]["elapsed_seconds"] * 1000 // (2 * len(commands)))
    preview.require(timeout > 0 and timeout * len(commands) <= 3600000, "verification plan exceeds supported duration")
    return {"schema": "louiselm.workspace.verification-plan/1", "commands": [
        {"argv": argv, "cwd": ".", "timeout_ms": timeout} for argv in commands
    ]}


def pending_result(manifest, run_id, policy, workload, digest):
    def route(arm):
        selected = manifest["routes"][arm]
        return {"agent": selected["agent"], "provider": selected["provider"], "model": selected["options"]["model"],
                "model_option_id": "model", "options": selected["options"]}
    fixtures = []
    for fixture in manifest["fixtures"]:
        acceptance = fixture["acceptance"]
        ids = [f"reference-{i+1}" for i in range(len(acceptance["reference_checks"]))]
        ids += [f"command-{i+1}" for i in range(len(acceptance["commands"]))]
        fixtures.append({"id": fixture["id"], "source": fixture["provenance"]["reference"], "digest": digest,
                         "checks": [{"id": check, "baseline": "pending", "candidate": "pending"} for check in ids or ["human-review"]],
                         "human_review_required": acceptance["human_review"] is not None})
    return {"report": {"version": 1, "id": run_id, "policy_revision": policy, "workload": workload,
                       "baseline": route("baseline"), "candidate": route("candidate"), "fixtures": fixtures},
            "observations": {"outcome": "unknown", "provider_requests": None, "api_cost": None, "quota": None,
                             "cleanup": "await_terminal_receipts"}}


def write_json(path, value):
    data = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".result-", delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def share(path, group):
    for child in [path, *path.rglob("*")]:
        preview.require(not child.is_symlink(), "refused unsafe prepared snapshot")
        os.chown(child, -1, group)
        child.chmod(0o750 if child.is_dir() else 0o640)


def run(selection_path, output):
    selected = read_selection(selection_path)
    path = Path(selected["manifest"])
    manifest = preview.read_manifest(path)
    preview.validate(manifest, path.parent)
    contents, prompts = selected_inputs(manifest, path.parent)
    parent = Path(selected["snapshot_parent"])
    metadata = parent.lstat()
    preview.require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == os.getuid()
                    and metadata.st_gid == selected["input_group"] and stat.S_IMODE(metadata.st_mode) == 0o750,
                    "snapshot_parent must be operator-owned, input-group-owned and mode 0750")
    for executable in ("nvim", "louiselm-skills", "louiselm-control"):
        preview.require(shutil.which(executable) is not None, f"unsupported: installed {executable} unavailable")
    preview.require(not output.exists() and not output.is_symlink() and output.parent.is_dir(), "output must be a new private directory")
    preview.require(output.resolve() != path.parent.resolve() and path.parent.resolve() not in output.resolve().parents,
                    "output must be outside the selected fixture bundle")
    output.mkdir(mode=0o700)
    with tempfile.TemporaryDirectory(prefix="qualification-", dir=parent) as temporary:
        owned = Path(temporary)
        os.chown(owned, -1, selected["input_group"])
        owned.chmod(0o750)
        checkout, snapshot = owned / "source", owned / "snapshot"
        commit = fixture_repository(contents, checkout)
        prepared = subprocess.run(["louiselm-skills", "workspace", "prepare", "--repository", str(checkout),
                                   "--output", str(snapshot), "--robot-json"], cwd="/", text=True,
                                  capture_output=True, timeout=30, check=False)
        preview.require(prepared.returncode == 0, "installed source snapshot preparation refused")
        binding = json.loads(prepared.stdout)
        preview.require(binding.get("base_commit") == commit, "source snapshot HEAD changed")
        snapshot_bytes = (snapshot / "snapshot.json").read_bytes()
        snapshot_record = json.loads(snapshot_bytes)
        expected = [{"path": name, "executable": False, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                    for name, data in sorted(contents.items())]
        preview.require(snapshot_record.get("files") == expected and binding.get("snapshot_digest") == "sha256:" + hashlib.sha256(snapshot_bytes).hexdigest(),
                        "snapshot does not contain exactly the selected fixture bytes")
        share(snapshot, selected["input_group"])
        plan = owned / "plan.json"
        write_json(plan, verification_plan(manifest))
        os.chown(plan, -1, selected["input_group"])
        plan.chmod(0o640)
        plan_digest = "sha256:" + hashlib.sha256(plan.read_bytes()).hexdigest()
        envelope = selected["envelope"]
        # A caller approves the exact curated commands. A different preselected
        # plan is never silently replaced under an old Run approval.
        preview.require(envelope.get("verification_plan_digest") == plan_digest, "envelope must name the exact generated verification plan digest")
        inputs = selected["input_manifest"]
        inputs["source_snapshot_digest"] = binding["snapshot_digest"]
        inputs["source_base_digest"] = binding["base_digest"]
        launch = {"schema": "louiselm.qualification-run/1", "manifest": manifest, "prompts": prompts,
                  "policy_revision": selected["policy_revision"], "workload": selected["workload"], "retention_days": 7,
                  "envelope": envelope, "input_manifest": inputs, "snapshot": str(snapshot),
                  "snapshot_digest": binding["snapshot_digest"], "base_commit": commit, "plan": str(plan), "cache": selected["cache"]}
        internal = owned / "launch.json"
        write_json(internal, launch)
        initial = pending_result(manifest, envelope["run_id"], selected["policy_revision"], selected["workload"], binding["snapshot_digest"])
        write_json(output / "report.json", initial["report"])
        write_json(output / "observations.json", initial["observations"])
        env = {**os.environ, "LOUISELM_QUALIFICATION_ROOT": str(ROOT), "LOUISELM_QUALIFICATION_SELECTION": str(internal),
               "LOUISELM_QUALIFICATION_OUTPUT": str(output), "XDG_STATE_HOME": str(owned / "state")}
        timeout = max(1, (envelope["expires_at_ms"] - time.time() * 1000) / 1000) + 10
        completed = subprocess.run(["nvim", "--headless", "--noplugin", "-u", "NONE", "--cmd",
                                   "lua vim.opt.runtimepath:prepend(vim.env.LOUISELM_QUALIFICATION_ROOT)",
                                   "-c", "lua require('louiselm.routing.trial_operator').start()"], cwd="/", env=env,
                                  capture_output=True, timeout=timeout, check=False)
        preview.require(completed.returncode == 0, "trial stopped; inspect pending report, observations and terminal launcher receipts")
    return output


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    plan = commands.add_parser("plan", help="print fixed plan digest; no Model or command execution")
    plan.add_argument("manifest", type=Path)
    launch = commands.add_parser("run", help="explicitly authorize and execute a selected paired trial")
    launch.add_argument("selection", type=Path)
    launch.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "plan":
            manifest = preview.read_manifest(args.manifest)
            preview.validate(manifest, args.manifest.parent)
            value = verification_plan(manifest)
            data = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
            print(json.dumps({"plan": value, "digest": "sha256:" + hashlib.sha256(data).hexdigest()}, sort_keys=True))
        else:
            output = run(args.selection, args.output.absolute())
            print(json.dumps({"report": str(output / "report.json"), "observations": str(output / "observations.json")}, sort_keys=True))
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired):
        print("qualification run refused or interrupted; inspect selected inputs and any pending result/launcher receipts", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
