#!/usr/bin/env python3
"""Private foreground no-Provider QA coordinator; never implicit authority."""
import hashlib
import json
import os
import stat
import tempfile
from pathlib import Path
import argparse
import copy
import fcntl
import select
import subprocess
import sys
import time
import uuid

PREFIX = Path("/usr/local/lib/louiselm/current/bin")

COMMAND = "printf QA_OK"
PLAN = {"schema": "louiselm.workspace.verification-plan/1", "commands": [
    {"argv": ["/usr/bin/printf", "QA_OK"], "cwd": ".", "timeout_ms": 5000}
]}


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


EMPTY_VIEW = digest(encoded({"schema": "louiselm.skills.instruction-view/1", "generation": None,
                             "agent": None, "packages": []}))


def records(proposal, manifest, uid, now):
    require(proposal.get("schema") == "louiselm.run.input-proposal/1"
            and manifest.get("schema") == "louiselm.session.input-manifest/3"
            and proposal.get("manifest_digest") == digest(encoded(manifest))
            and proposal.get("envelope") == manifest.get("envelope")
            and proposal.get("envelope", {}).get("revision") == 1
            and manifest.get("skills") == {"mode": "off", "view_digest": EMPTY_VIEW}
            and manifest.get("isolation", {}).get("network") == "denied"
            and manifest.get("agent", {}).get("tool_integration") == "louiselm.test-tool-integration/1"
            and all(manifest.get(field) == [] for field in ("tool_schemas", "plugin_schemas", "acp_mcp_servers"))
            and type(uid) is int and uid > 0 and type(now) is int and now > 0,
            "QA requires exact owner-produced off/denied deterministic inputs")
    expires = now + 3_600_000
    commands = {"command_digest": digest(COMMAND.encode()), "timeout_ms": 5000,
                "uses": 2, "allow_delegation": False, "expires_at_ms": expires}
    envelope = {"schema": "louiselm.broker.run-envelope/2", "run_id": proposal["run_id"],
                "envelope_id": proposal["envelope"]["id"], "envelope_revision": 1,
                "controller_uid": uid, "bead_scope": None, "provider_requests": None,
                "commands": commands, "verification_plan_digest": digest(encoded(PLAN)),
                "max_sessions": 12, "expires_at_ms": expires}
    grants = []
    for index in range(12):
        session = f'{proposal["run_id"]}-qa-{index + 1:02}'
        # This order is the launch request's closed Rust canonical wire order.
        request = {"schema": "louiselm.launch.request/3", "protocol_version": 1,
                   "request_id": session + "-r", "authorization_id": session + "-a",
                   "session_id": session, "run_id": proposal["run_id"],
                   "agent_id": manifest["agent"]["id"], "envelope_id": envelope["envelope_id"],
                   "envelope_revision": 1, "skills": manifest["skills"],
                   "session_input_manifest_id": proposal["manifest_digest"]}
        grants.append({"role": "agent", "request": request, "controller_uid": uid,
                       "expires_at_ms": expires, "broker_loss_grace_ms": 5000,
                       "require_cold_recovery": False,
                       "conformance": {"attendance": "interactive", "waiver": None},
                       "dependencies": None, "commands": commands, "skill_requests": None,
                       "beads_mutations": None, "provider_requests": None})
    return {"envelope": envelope, "grants": grants}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_private(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1 and metadata.st_uid == os.getuid()
                and stat.S_IMODE(metadata.st_mode) == 0o600 and metadata.st_size <= 65536,
                "QA record must be operator-owned, unlinked, regular, mode 0600 and bounded")
        data = stream.read(65537)
        after = os.fstat(stream.fileno())
        require(len(data) == metadata.st_size and metadata.st_ctime_ns == after.st_ctime_ns,
                "QA record changed during read")
        return json.loads(data)


def write_private(path, value):
    # The caller holds the single journal lock; publication is durable before effects.
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".qa-", delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(encoded(value))
            output.flush()
            os.fsync(output.fileno())
            os.replace(temporary, path)
            directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            temporary.unlink(missing_ok=True)


class Journal:
    """Owns one foreground Run; uncertainty never makes its active slot reusable."""
    def __init__(self, path, value):
        self.path, self.value = path, value

    @classmethod
    def create(cls, path):
        require(not path.exists(), "QA journal already exists")
        value = {"schema": "louiselm.qa.journal/1", "next": 0, "active": None,
                 "spent": 0, "pending": None, "envelope_digest": None, "selection_digest": None}
        write_private(path, value)
        return cls(path, value)

    @classmethod
    def open(cls, path):
        value = read_private(path)
        require(set(value) == {"schema", "next", "active", "spent", "pending", "envelope_digest", "selection_digest"}
                and value["schema"] == "louiselm.qa.journal/1"
                and type(value["next"]) is int and 0 <= value["next"] <= 12
                and (value["active"] is None or type(value["active"]) is int
                     and 0 <= value["active"] < value["next"])
                and type(value["spent"]) is int and 0 <= value["spent"] <= 2,
                "QA journal is malformed; retained uncertainty is not a new Run")
        return cls(path, value)

    def save(self):
        write_private(self.path, self.value)

    def reserve(self, now, expires):
        require(now < expires and self.value["next"] < 12 and self.value["active"] is None
                and self.value["pending"] is None, "QA expired, full, active or uncertain; no next Session")
        index = self.value["next"]
        self.value.update(next=index + 1, active=index, spent=0, pending="authorization")
        self.save()
        return index

    def terminal(self, receipt):
        payload = receipt.get("payload", {})
        require(self.value["active"] is not None and payload.get("resulting_state") == "terminal"
                and payload.get("outcome", {}).get("action") == "disposal",
                "Only authenticated confirmed terminal disposal releases the QA slot")
        self.value.update(active=None, pending=None, spent=0)
        self.save()


def cli(binary, arguments, value=None):
    result = subprocess.run([str(PREFIX / binary), *arguments],
                            input=None if value is None else encoded(value),
                            capture_output=True, timeout=40, check=False)
    require(result.returncode == 0 and 0 < len(result.stdout) <= 3_000_000,
            f"Installed {binary} {' '.join(arguments[:2])} refused or became unavailable; no automatic retry")
    return json.loads(result.stdout)


def inspect_inputs(input_path, expected):
    proposal = cli("louiselm-skills", ["workspace", "launch-inputs", "inspect-proposal",
                                     "--input", str(input_path), "--digest", expected, "--robot-json"])
    manifest_bytes = (input_path / "manifest.json").read_bytes()
    require(len(manifest_bytes) <= 65536 and digest(manifest_bytes) == expected,
            "Owner-produced manifest changed")
    return proposal, json.loads(manifest_bytes)


def validate_selection(selection):
    require(set(selection) == {"schema", "input", "manifest_digest", "release_ids", "envelope", "grants"}
            and selection["schema"] == "louiselm.qa.selection/1"
            and Path(selection["input"]).is_absolute()
            and len(selection["release_ids"]) == 2 and len(set(selection["release_ids"])) == 2
            and all(isinstance(value, str) and len(value) == 71 and value.startswith("sha256:")
                    and all(c in "0123456789abcdef" for c in value[7:]) for value in selection["release_ids"]),
            "QA selection requires two exact reviewed fresh releases")
    proposal, manifest = inspect_inputs(Path(selection["input"]), selection["manifest_digest"])
    expected = records(proposal, manifest, os.getuid(), selection["envelope"]["expires_at_ms"] - 3_600_000)
    require(selection["envelope"] == expected["envelope"] and selection["grants"] == expected["grants"],
            "QA authority was substituted or widened")
    return proposal, manifest


def prepare(args):
    proposal, manifest = inspect_inputs(args.input, args.digest)
    selection = {"schema": "louiselm.qa.selection/1", "input": str(args.input),
                 "manifest_digest": args.digest, "release_ids": args.releases,
                 **records(proposal, manifest, os.getuid(), int(time.time() * 1000))}
    validate_selection(selection)
    args.output.mkdir(mode=0o700)
    write_private(args.output / "selection.json", selection)
    write_private(args.output / "plan.json", PLAN)
    print("Inactive selection:", args.output / "selection.json")
    print("Review digest:", digest(encoded(selection)))
    print("Expiry is fixed now. Nothing staged, approved, signed, installed or launched.")


def binding(value, request):
    return (value.get("session_id") == request["session_id"] and value.get("run_id") == request["run_id"]
            and value.get("envelope_revision") == request["envelope_revision"])


def receipt_for(response, request):
    receipt = response.get("Ok", {})
    payload = receipt.get("payload", {})
    require(binding(payload, request) and payload.get("request_id") == request["request_id"]
            and payload.get("outcome", {}).get("action") == request["action"]
            and payload.get("resulting_state") == {"park": "parked", "resume": "running", "disposal": "terminal"}[request["action"]]
            and isinstance(receipt.get("signature"), str) and bool(receipt["signature"]),
            "Authenticated lifecycle result refused or did not bind the exact request")
    return receipt


def status_for(request):
    status = cli("louiselm-control", ["session", "inspect", request["session_id"], "--json"])
    require(binding(status, request), "Status named another Session or revision")
    return status


def evidence(directory, kind, value):
    # Only typed status/receipt/conformance data enters this directory, never tool output.
    require(len(list(directory.iterdir())) < 512, "Evidence bound reached; retain this round and stop")
    write_private(directory / f"{kind}-{uuid.uuid4()}.json", value)


def lifecycle(journal, directory, request, action):
    status = status_for(request)
    require(action in status["allowed_actions"] and status["broker_head"] is not None
            and status["broker_head"] == status["launcher_head"] and status["pending_receipt_count"] == 0,
            "Lifecycle is unavailable or its receipt head is unsettled")
    operation = {"schema": "louiselm.launch.lifecycle-request/1", "protocol_version": 1,
                 "request_id": str(uuid.uuid4()), "session_id": request["session_id"], "run_id": request["run_id"],
                 "authorization_id": request["authorization_id"], "action": action, "expected_state": status["state"],
                 "expected_receipt_sequence": status["broker_head"]["sequence"],
                 "envelope_revision": request["envelope_revision"]}
    journal.value["pending"] = operation
    journal.save()
    response = cli("louiselm-control", ["lifecycle", "--json"], operation)
    receipt = receipt_for(response, operation)
    evidence(directory, action, receipt)
    if action != "disposal":
        journal.value["pending"] = None
        journal.save()
    return receipt


def resolve_disposal(journal, directory, request):
    operation = journal.value["pending"]
    if isinstance(operation, dict) and operation.get("action") == "disposal":
        require(binding(operation, request) and operation.get("authorization_id") == request["authorization_id"]
                and operation.get("schema") == "louiselm.launch.lifecycle-request/1"
                and operation.get("protocol_version") == 1,
                "Retained disposal does not bind this QA Session")
        # Explicit foreground resolution only; never replay commands, Park, Resume or authorization.
        receipt = receipt_for(cli("louiselm-control", ["lifecycle", "--json"], operation), operation)
        evidence(directory, "disposal-resolution", receipt)
    else:
        receipt = lifecycle(journal, directory, request, "disposal")
    status = status_for(request)
    require(status["state"] == "terminal" and status["broker_head"] is not None
            and status["broker_head"] == status["launcher_head"] and status["pending_receipt_count"] == 0,
            "Terminal disposal is not settled; retain active identity")
    journal.terminal(receipt)
    print("Terminal disposal resolved; original expiry and authorization count unchanged")


def read_line(stream, deadline):
    line = bytearray()
    while time.monotonic() < deadline and len(line) <= 65536:
        ready, _, _ = select.select([stream], [], [], max(0, deadline - time.monotonic()))
        require(ready, "QA Agent reply timed out; spending and delivery remain uncertain")
        byte = os.read(stream.fileno(), 1)
        require(byte, "QA Agent disconnected; no automatic replay")
        if byte == b"\n":
            return bytes(line)
        line.extend(byte)
    raise ValueError("QA Agent reply unavailable or oversized")


def command(journal, process, request, expires):
    require(int(time.time() * 1000) < expires and journal.value["spent"] < 2
            and journal.value["pending"] is None and status_for(request)["state"] == "running",
            "QA command expired, spent, suspended or uncertain")
    journal.value["spent"] += 1
    sequence = journal.value["spent"]
    operation = {"schema": "louiselm.launch.tool-execution/1", "protocol_version": 1,
                 "request_id": f"qa-command-{sequence}", "session_id": request["session_id"],
                 "run_id": request["run_id"], "envelope_revision": 1, "sequence": sequence,
                 "command": COMMAND, "timeout_ms": 5000}
    journal.value["pending"] = operation
    journal.save()  # A crash cannot make an uncertain command unspent.
    process.stdin.write(b"\x1e" + encoded(operation) + b"\n")
    process.stdin.flush()
    line = read_line(process.stdout, time.monotonic() + 15)
    require(line.startswith(b"\x1e"), "QA Agent returned an unexpected protocol")
    reply = json.loads(line[1:])
    require(binding(reply, request) and reply.get("request_id") == operation["request_id"],
            "Tool reply named another command or Session")
    outcome = reply.get("operation", {}).get("outcome", {})
    require(outcome == {"kind": "completed", "output": {"exit_code": 0, "stdout": "QA_OK", "stderr": "",
                                                         "truncated": False, "timed_out": False}},
            "Exact QA_OK result not established; no tool payload is logged")
    journal.value["pending"] = None
    journal.save()
    print("QA_OK; spent", sequence, "of 2 (unchanged by Resume)")


def run(args):
    require(args.exclusive_enforced_window, "Operator must confirm the reviewed exclusive enforcement window")
    selection = read_private(args.selection)
    selected_digest = digest(encoded(selection))
    require(args.reviewed == selected_digest, "Exact selection has not been reviewed")
    _, manifest = validate_selection(selection)
    root = args.selection.parent
    metadata = root.lstat()
    require(stat.S_ISDIR(metadata.st_mode) and stat.S_IMODE(metadata.st_mode) == 0o700 and metadata.st_uid == os.getuid(),
            "QA artifact directory must be private and operator-owned")
    descriptor = os.open(root / "lock", os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    with os.fdopen(descriptor, "wb") as lock:
        metadata = os.fstat(lock.fileno())
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1 and metadata.st_uid == os.getuid()
                and stat.S_IMODE(metadata.st_mode) == 0o600, "QA lock must be private and unlinked")
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        journal_path = root / "journal.json"
        journal = Journal.open(journal_path) if journal_path.exists() else Journal.create(journal_path)
        require(journal.value["selection_digest"] in (None, selected_digest), "Original QA selection changed")
        journal.value["selection_digest"] = selected_digest
        journal.save()
        directory = root / "evidence"
        directory.mkdir(mode=0o700, exist_ok=True)
        metadata = directory.lstat()
        require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == os.getuid()
                and stat.S_IMODE(metadata.st_mode) == 0o700, "Evidence directory must be private")
        if args.verb == "resolve-disposal":
            require(journal.value["active"] is not None, "No retained active QA identity to resolve")
            resolve_disposal(journal, directory, selection["grants"][journal.value["active"]]["request"])
            return
        require(journal.value["active"] is None and journal.value["pending"] is None,
                "Retained active/uncertain Session blocks restart; inspect and explicitly resolve disposal")
        interactive(selection, manifest, journal, directory)


def authorize(selection, manifest, journal, directory):
    identity = cli("louiselm-skills", ["release", "identity", "--robot-json"])
    installation = cli("louiselm-skills", ["workspace", "qa-runtime", "inspect-installation", "--robot-json"])
    require(identity.get("verified") is True and identity.get("release_id") in selection["release_ids"]
            and installation.get("release_id") == identity.get("release_id")
            and installation.get("conformance") == "enforced",
            "QA needs one of its reviewed installed releases and enforced admission")
    index = journal.reserve(int(time.time() * 1000), selection["envelope"]["expires_at_ms"])
    grant = selection["grants"][index]
    request = grant["request"]
    if journal.value["envelope_digest"] is None:
        approval = cli("louiselm-control", ["run", "authorize", "--json"],
                       {"kind": "run", "envelope": selection["envelope"]})
        require(approval.get("kind") == "run" and approval["receipt"]["run_id"] == request["run_id"],
                "Run approval reply mismatch")
        journal.value["envelope_digest"] = approval["receipt"]["envelope_digest"]
        require(journal.value["envelope_digest"] == digest(encoded(selection["envelope"])), "Exact Run approval mismatch")
        journal.save()
        evidence(directory, "run-approval", approval)
    source = Path(selection["input"])
    inputs = inspect_inputs(source, selection["manifest_digest"])[0]
    if index == 0:
        staged = cli("louiselm-control", ["launch-inputs", "stage", "--json"],
                     {"manifest": manifest, "snapshot": str(source / "snapshot"), "cache": str(source / "cache"),
                      "expected_base_commit": inputs["base_commit"]})
        require(staged.get("manifest_digest") == selection["manifest_digest"], "Staged manifest mismatch")
    child = cli("louiselm-control", ["run", "authorize", "--json"],
                {"kind": "session", "grant": grant, "expected_envelope_digest": journal.value["envelope_digest"]})
    require(child.get("kind") == "session" and binding(child["receipt"], request)
            and child["receipt"]["authorization_id"] == request["authorization_id"]
            and child["receipt"]["envelope_digest"] == journal.value["envelope_digest"]
            and child["receipt"]["request_digest"] == digest(encoded(request)), "Child authorization mismatch")
    evidence(directory, "child-approval", child)
    # The exact canonical newline-terminated request `louiselm-launch prepare` reads for a waiver.
    with open(os.open(directory / f"launch-{request['session_id']}.json",
                      os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as stream:
        stream.write(encoded(request) + b"\n")
    return request


def launch(journal, directory, request):
    # Recorded before the effect: a refused or lost launch is never a certain no-op.
    journal.value["pending"] = "launch"
    journal.save()
    log = directory / f"launcher-{request['session_id']}.log"
    with open(os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as stderr:
        process = subprocess.Popen(["/usr/bin/sudo", "-n", str(PREFIX / "louiselm-launch"), "run"],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0)
    process.stdin.write(encoded(request) + b"\n" + b"READY\n")
    process.stdin.flush()
    try:
        require(read_line(process.stdout, time.monotonic() + 20) == b"READY", "QA launch was not acknowledged")
    except ValueError as error:
        # The launcher's stderr is payload-free (typed SupervisorError); show its category.
        reason = log.read_bytes()[-512:].decode(errors="replace").strip().splitlines()
        raise ValueError(f"{error}: {reason[-1] if reason else 'no launcher diagnostic'}") from error
    journal.value["pending"] = None
    journal.save()
    return process


def interactive(selection, manifest, journal, directory):
    process, request = None, None
    try:
        print("No Provider. No policy/signing/release operation. Verbs: start (= authorize + launch), authorize, launch, status, command, park, resume, dispose, quit.")
        for line in sys.stdin:
            verb = line.strip()
            before = copy.deepcopy(journal.value)
            try:
                if verb == "quit":
                    require(request is None, "Dispose the active Session before quitting")
                    return
                if verb in ("start", "authorize"):
                    require(request is None, "Dispose the active Session before authorizing another")
                    request = authorize(selection, manifest, journal, directory)
                    if verb == "authorize":
                        print("Authorized", request["session_id"], "launch request", directory / f"launch-{request['session_id']}.json")
                if verb in ("start", "launch"):
                    require(request is not None and process is None, "Authorize a Session before launching it")
                    process = launch(journal, directory, request)
                    print("Running", request["session_id"])
                if verb == "status":
                    require(request is not None, "No active QA Session")
                    status = status_for(request)
                    report = cli("louiselm-control", ["session", "conformance", request["session_id"], "--json"])
                    require(report.get("session_id") == request["session_id"] and report.get("run_id") == request["run_id"],
                            "Conformance report mismatch")
                    if report.get("report") is not None:
                        require(digest(report["report"].encode()) == report["admission"].get("report_digest"),
                                "Conformance report bytes do not match immutable admission")
                    evidence(directory, "status", status)
                    evidence(directory, "conformance", report)
                    print("State", status["state"], "last_check", report.get("last_check"))
                elif verb == "command":
                    require(process is not None and request is not None, "No active QA Session")
                    command(journal, process, request, selection["envelope"]["expires_at_ms"])
                elif verb in ("park", "resume", "dispose"):
                    require(request is not None, "No active QA Session")
                    action = "disposal" if verb == "dispose" else verb
                    receipt = lifecycle(journal, directory, request, action)
                    if verb == "dispose":
                        process.wait(timeout=5)
                        journal.terminal(receipt)
                        request, process = None, None
                    elif verb == "resume":
                        process.stdin.write(b"\x1f")
                        process.stdin.flush()
                        require(read_line(process.stdout, time.monotonic() + 5) == b"\x1f", "Explicit reconnect failed")
                    print(verb, "confirmed")
                elif verb not in ("start", "authorize", "launch"):
                    raise ValueError("Unknown QA verb; no arbitrary shell or Agent prompt is accepted")
            except ValueError as error:
                # A refusal that recorded nothing (typo, pre-effect check) cannot be uncertain;
                # keep the Session controllable. Any recorded effect still stops here (louiselm-c327f).
                if journal.value != before:
                    raise
                print("Refused:", error)

        require(request is None, "Foreground input ended with an active Session; retained journal blocks another")
    finally:
        # Closing transport is not proof of cleanup and does not free the journal.
        if process is not None:
            process.stdin.close()
            process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="verb", required=True)
    proposal = commands.add_parser("prepare")
    proposal.add_argument("--input", type=Path, required=True)
    proposal.add_argument("--digest", required=True)
    proposal.add_argument("--releases", nargs=2, required=True)
    proposal.add_argument("--output", type=Path, required=True)
    for verb in ("run", "resolve-disposal"):
        execute = commands.add_parser(verb)
        execute.add_argument("selection", type=Path)
        execute.add_argument("--reviewed", required=True)
        execute.add_argument("--exclusive-enforced-window", action="store_true")
    args = parser.parse_args()
    try:
        (prepare if args.verb == "prepare" else run)(args)
        return 0
    except ValueError as error:
        print(str(error), file=sys.stderr)
        print("Conformance QA refused; retain artifacts. No automatic retry.", file=sys.stderr)
        return 1
    except (KeyError, TypeError, OSError, subprocess.SubprocessError):
        print("Conformance QA refused or interrupted; retain artifacts and inspect the owning Session. No automatic retry.",
              file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
