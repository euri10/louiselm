"""Full stock-Codex ACP chain under the disposable exact-sender guard."""

import ctypes as C
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import threading
import time

from binding_probe import BindingGuard, Grant
from endpoint_probe import sse


SESSION_UID = 4_020_000
EXPECTED = {
    "/var/tmp/integration-fixture/nvim": "cce0a9494c07dad5eef2fc2b10a81ca7bb447c142829c3a4767452fac74228d3",
    "/var/tmp/integration-fixture/acp-proxy": "d1885d617c52169c92a124172bc160be413a11fa5478706f9e6e0dbcceff9e0a",
    "/var/tmp/integration-fixture/node": "b2959781cc5a74c357ffa02367efa8a0330cbb1c9cb347732fdfaaaca381cbcd",
    "/var/tmp/integration-fixture/codex-acp.js": "3f2359fe5584c545eb6c0db688cf9805bde415cf531f986875adbb1060749bd8",
    "/var/tmp/integration-fixture/sqlite3": "01e2610becea4b6e85b14e905a05ccfaf418f4b715f3c7641fc2fd5fc9df5da9",
    "/var/tmp/ow3ok-codex": "56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da",
}


class Decision(C.Structure):
    _fields_ = [
        ("pid_tgid", C.c_uint64),
        ("reason", C.c_uint32),
        ("family", C.c_uint32),
        ("port", C.c_uint32),
        ("socket_netns", C.c_uint32),
        ("task_netns", C.c_uint32),
    ]


def require_guest():
    assert sys.argv[1] == "--disposable-vm" and os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"


def isolate_network():
    os.unshare(os.CLONE_NEWNET)
    subprocess.run(["/usr/bin/ip", "link", "set", "lo", "up"], check=True, capture_output=True)
    interfaces = [name for _index, name in socket.if_nameindex()]
    assert interfaces == ["lo"], interfaces
    return {"interfaces": interfaces, "ambient_routes": False}


def digest(path):
    with open(path, "rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def install_io_uring_refusal():
    """Install an inherited seccomp rule for the three io_uring syscalls."""
    class Filter(C.Structure):
        _fields_ = [("code", C.c_ushort), ("jt", C.c_ubyte), ("jf", C.c_ubyte), ("k", C.c_uint32)]

    class Program(C.Structure):
        _fields_ = [("length", C.c_ushort), ("filters", C.POINTER(Filter))]

    rules = [Filter(0x20, 0, 0, 0)]
    for syscall_number in (425, 426, 427):
        rules.extend((Filter(0x15, 0, 1, syscall_number), Filter(0x06, 0, 0, 0x0005_0001)))
    rules.append(Filter(0x06, 0, 0, 0x7FFF_0000))
    filters = (Filter * len(rules))(*rules)
    program = Program(len(rules), filters)
    libc = C.CDLL(None, use_errno=True)
    assert libc.prctl(38, 1, 0, 0, 0) == 0
    assert libc.prctl(22, 2, C.byref(program)) == 0, C.get_errno()


def process_info(pid):
    status = Path(f"/proc/{pid}/status").read_text().splitlines()
    parent = int(next(line for line in status if line.startswith("PPid:")).split()[1])
    command = Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
    executable = os.path.realpath(f"/proc/{pid}/exe")
    return parent, executable, command


def is_descendant(pid, ancestor):
    while pid > 1:
        if pid == ancestor:
            return True
        try:
            pid = process_info(pid)[0]
        except (FileNotFoundError, ProcessLookupError):
            return False
    return False


def find_runtime(ancestor):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        matches = []
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            pid = int(entry.name)
            try:
                if os.path.samefile(entry / "exe", "/var/tmp/ow3ok-codex") and is_descendant(pid, ancestor):
                    matches.append(pid)
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                pass
        if len(matches) == 1:
            return matches[0]
        assert len(matches) <= 1, matches
        time.sleep(0.02)
    raise AssertionError("one measured Codex app-server did not appear")


def stop_process(process):
    if process is None or process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(5)


def collect_strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from collect_strings(item)
    elif isinstance(value, dict):
        for item in value.values():
            yield from collect_strings(item)


def tool_output(completed):
    assert completed.returncode == 0, completed
    marker = "LOUISELM_TOOL_PROBE "
    line = next(line for line in completed.stdout.splitlines() if line.startswith(marker))
    return json.loads(line[len(marker):])


class ModelEndpoint:
    def __init__(self):
        self.requests = []
        self.errors = []
        self.runtime_pid = None
        self.tool_report = None
        self.request_shape = None
        self.first_received = threading.Event()
        self.first_release = threading.Event()
        endpoint = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    length = int(self.headers["Content-Length"])
                    assert 0 < length <= 1_000_000
                    body = json.loads(self.rfile.read(length))
                    endpoint.requests.append({
                        "path": self.path,
                        "authorization": "Authorization" in self.headers,
                        "content_length": length,
                    })
                    output = {
                        "type": "message",
                        "id": "msg_integration",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": "OFFLINE_ACP_GUARD_OK",
                            "annotations": [],
                        }],
                    }
                    if len(endpoint.requests) == 1:
                        assert endpoint.runtime_pid is not None
                        names = [tool.get("name") for tool in body.get("tools", [])]
                        endpoint.request_shape = {
                            "keys": sorted(body),
                            "model": body.get("model"),
                            "tool_names": names,
                        }
                        command = (
                            f"python3 /var/tmp/integration_tool.py "
                            f"{endpoint.runtime_pid} {self.server.server_port}"
                        )
                        if "exec_command" in names:
                            name, arguments = "exec_command", {"cmd": command, "max_output_tokens": 1000}
                        elif "shell_command" in names:
                            name, arguments = "shell_command", {"command": command, "timeout_ms": 5000}
                        elif "shell" in names:
                            name, arguments = "shell", {"command": ["/bin/sh", "-c", command], "timeout_ms": 5000}
                        else:
                            endpoint.errors.append(f"no supported fixture shell: {names}")
                            name = None
                        if name is not None:
                            output = {
                                "type": "function_call",
                                "id": "fc_integration",
                                "call_id": "call_integration",
                                "name": name,
                                "arguments": json.dumps(arguments),
                            }
                        endpoint.first_received.set()
                        assert endpoint.first_release.wait(10), "first request barrier timed out"
                    else:
                        for value in collect_strings(body):
                            marker = "LOUISELM_TOOL_PROBE "
                            if marker in value:
                                candidate = value.split(marker, 1)[1].splitlines()[0]
                                endpoint.tool_report = json.loads(candidate)
                                break
                        assert endpoint.tool_report is not None
                    data = sse(output)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except BaseException as error:
                    endpoint.errors.append(repr(error))
                    self.close_connection = True

        class Server(http.server.ThreadingHTTPServer):
            def handle_error(self, _request, _client_address):
                pass

        self.server = Server(("127.0.0.1", 0), Handler)
        self.worker = threading.Thread(target=self.server.serve_forever)
        self.worker.start()

    def close(self):
        self.first_release.set()
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(5)
        assert not self.worker.is_alive()


def enroll_nonchild(guard, pid):
    pin = os.pidfd_open(pid)
    guard.pins.append(pin)
    signal.pidfd_send_signal(pin, signal.SIGSTOP)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        state = next(line for line in Path(f"/proc/{pid}/status").read_text().splitlines() if line.startswith("State:"))
        if "T (stopped)" in state:
            break
        time.sleep(0.01)
    else:
        raise AssertionError("Codex runtime did not stop for measurement")
    try:
        assert os.path.samefile(f"/proc/{pid}/exe", "/var/tmp/ow3ok-codex")
        guard.put("tasks", C.c_int(pin), Grant(1, 101, 201, 0), 1)
    finally:
        signal.pidfd_send_signal(pin, signal.SIGCONT)


def guard_decision(guard):
    descriptor = guard.lib.bpf_object__find_map_fd_by_name(guard.obj, b"decision")
    if descriptor < 0:
        return None
    key, value = C.c_uint32(0), Decision()
    assert guard.lib.bpf_map_lookup_elem(descriptor, C.byref(key), C.byref(value)) == 0
    return {name: getattr(value, name) for name, _kind in value._fields_}


def chain(runtime_pid, nvim_pid):
    observed = []
    pid = runtime_pid
    while True:
        parent, executable, command = process_info(pid)
        observed.append({"pid": pid, "executable": executable, "command": command})
        if pid == nvim_pid:
            break
        pid = parent
    expected = [
        "/var/tmp/ow3ok-codex",
        "/var/tmp/integration-fixture/node",
        "/var/tmp/integration-fixture/acp-proxy",
        "/var/tmp/integration-fixture/nvim",
    ]
    assert [item["executable"] for item in observed] == expected, observed
    assert "/var/tmp/integration-fixture/codex-acp.js" in observed[1]["command"]
    return (
        [{"executable": item["executable"], "sha256": digest(item["executable"])} for item in observed],
        [item["pid"] for item in observed],
    )


def main():
    require_guest()
    allow_io_uring = "--allow-io-uring" in sys.argv[3:]
    network = isolate_network()
    for path, expected in EXPECTED.items():
        assert digest(path) == expected, path

    integration = Path("/var/tmp/integration")
    integration.mkdir(mode=0o700, exist_ok=True)
    for name in ("home", "proxy", "state", "usage", "workspace"):
        directory = integration / name
        directory.mkdir(mode=0o700, exist_ok=True)
        os.chown(directory, SESSION_UID, SESSION_UID)
    for stale in ("driver-ready", "enabled", "result.json"):
        try:
            (integration / stale).unlink()
        except FileNotFoundError:
            pass
    os.chown(integration, SESSION_UID, SESSION_UID)

    endpoint = ModelEndpoint()
    guard = BindingGuard(sys.argv[2])
    process = None
    descendants = []
    report = {
        "schema": "louiselm.codex-guard-acp-integration/1",
        "verdict": "INCOMPLETE_NOT_VERIFIED",
        "kernel": os.uname().release,
        "io_uring": "refused by inherited seccomp filter",
    }
    try:
        guard.reserve(endpoint.server)
        process = subprocess.Popen(
            [
                "/var/tmp/integration-fixture/nvim", "--headless", "--clean", "-u", "NONE", "-l",
                "/var/tmp/integration_driver.lua", "/var/tmp/integration-louiselm",
                str(endpoint.server.server_port), str(integration / "result.json"),
                str(integration / "enabled"),
            ],
            cwd=integration / "workspace",
            env={
                "HOME": str(integration / "home"),
                "PATH": "/var/tmp/integration-fixture:/usr/bin:/bin",
                "USER": "fixture",
                "VIMRUNTIME": "/var/tmp/integration-fixture/runtime",
                "XDG_STATE_HOME": str(integration / "state"),
            },
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            user=SESSION_UID,
            group=SESSION_UID,
            extra_groups=[],
            preexec_fn=None if allow_io_uring else install_io_uring_refusal,
        )
        runtime_pid = find_runtime(process.pid)
        endpoint.runtime_pid = runtime_pid
        observed_chain, chain_pids = chain(runtime_pid, process.pid)
        for item in observed_chain:
            assert EXPECTED[item["executable"]] == item["sha256"]
        assert digest("/var/tmp/integration-fixture/codex-acp.js") == EXPECTED["/var/tmp/integration-fixture/codex-acp.js"]

        deadline = time.monotonic() + 30
        while not (integration / "driver-ready").exists():
            assert process.poll() is None
            assert time.monotonic() < deadline, "LouiseLM session did not become ready"
            time.sleep(0.02)

        enroll_nonchild(guard, runtime_pid)
        guard.publish(endpoint.server)
        (integration / "enabled").write_text("enabled\n")
        os.chown(integration / "enabled", SESSION_UID, SESSION_UID)
        assert endpoint.first_received.wait(10), "stock Codex did not reach the guarded endpoint"
        concurrent = subprocess.run(
            [sys.executable, "/var/tmp/integration_tool.py", str(runtime_pid), str(endpoint.server.server_port)],
            user=SESSION_UID,
            group=SESSION_UID,
            extra_groups=[],
            capture_output=True,
            text=True,
            timeout=5,
        )
        concurrent_report = tool_output(concurrent)
        assert all(item["denied"] for item in concurrent_report.values()), concurrent_report
        endpoint.first_release.set()

        stdout, stderr = process.communicate(timeout=55)
        assert process.returncode == 0, (stdout.decode(errors="replace"), stderr.decode(errors="replace"))
        driver = json.loads((integration / "result.json").read_text())
        assert driver["ready"] and driver["prompt_completed"] and driver["disposed"], (
            driver,
            guard_decision(guard),
            endpoint.requests,
            endpoint.errors,
        )
        assert not driver["errors"], driver
        assert not endpoint.errors, (endpoint.errors, endpoint.request_shape)
        assert len(endpoint.requests) == 2, (endpoint.requests, endpoint.request_shape)
        assert all(item["path"] == "/v1/responses" for item in endpoint.requests)
        assert not any(item["authorization"] for item in endpoint.requests)
        assert endpoint.tool_report is not None
        assert all(item["denied"] for item in endpoint.tool_report.values()), endpoint.tool_report

        sibling = subprocess.run(
            [sys.executable, "/var/tmp/integration_tool.py", str(runtime_pid), str(endpoint.server.server_port)],
            user=SESSION_UID,
            group=SESSION_UID,
            extra_groups=[],
            capture_output=True,
            text=True,
            timeout=5,
        )
        sibling_report = tool_output(sibling)
        assert all(item["denied"] for item in sibling_report.values()), sibling_report
        cleanup_deadline = time.monotonic() + 5
        descendants = [pid for pid in chain_pids if Path(f"/proc/{pid}").exists()]
        while descendants and time.monotonic() < cleanup_deadline:
            time.sleep(0.01)
            descendants = [pid for pid in chain_pids if Path(f"/proc/{pid}").exists()]
        assert not descendants, descendants
        logs = list((integration / "proxy").rglob("*.jsonl"))
        log_text = "".join(path.read_text(errors="replace") for path in logs)
        assert "Bearer " not in log_text and "Authorization" not in log_text
        report.update({
            "verdict": "STOCK_CODEX_ACP_GUARD_INTEGRATION_PASS_NOT_VERIFIED",
            "chain": observed_chain,
            "adapter_sha256": EXPECTED["/var/tmp/integration-fixture/codex-acp.js"],
            "requests": endpoint.requests,
            "tool_separation": endpoint.tool_report,
            "concurrent_sibling": concurrent_report,
            "after_runtime_exit": sibling_report,
            "network_namespace": network,
            "driver": {key: driver.get(key) for key in ("ready", "prompt_completed", "tool_started", "tool_finished", "disposed")},
            "logging": {"files": len(logs), "sensitive_headers_absent": True},
            "cleanup": {"descendants": 0, "guard_released_after_endpoint": True},
        })
        print(json.dumps(report, indent=2))
    finally:
        stop_process(process)
        endpoint.close()
        guard.close()


if __name__ == "__main__":
    main()
