#!/usr/bin/env python3
"""Offline negative feasibility proof for louiselm-qbr.5.1.3.5.

Run with the explicit Codex 0.153.4 executable path. Exit 0 means the
counterexample reproduced, NOT that endpoint authority is safe. No host home,
credentials, repository or external network is visible inside the namespace.
"""

import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import selectors
import socket
import subprocess
import sys
import threading
import time


def replay(port, fd=None):
    """A separate process replays only synthetic fixture request bytes."""
    request = json.loads(Path("/work/request.json").read_text())
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
    if fd is not None:
        connection.sock = socket.socket(fileno=fd)
        connection.sock.settimeout(5)
    connection.request("POST", "/v1/responses", request["body"].encode(), request["headers"])
    response = connection.getresponse()
    accepted = response.status == 200 and b"OFFLINE_PROBE_OK" in response.read()
    connection.close()
    print(json.dumps({"pid": os.getpid(), "accepted": accepted}), flush=True)
    return accepted


def sse(output):
    events = [
        {"type": "response.created", "response": {"id": "resp_probe"}},
        {"type": "response.output_item.added", "output_index": 0, "item": output},
        {"type": "response.output_item.done", "output_index": 0, "item": output},
        {"type": "response.completed", "response": {
            "id": "resp_probe", "status": "completed", "output": [output],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}},
    ]
    return "".join("event: " + event["type"] + "\ndata: " + json.dumps(event)
                   + "\n\n" for event in events).encode()


def experiment():
    assert os.readlink("/proc/self/ns/net") != sys.argv[2], "network isolation missing"
    assert not any(Path(os.environ["HOME"]).iterdir()), "home must start empty"
    assert [row.split()[0] for row in Path("/proc/net/route").read_text().splitlines()[1:]] == []
    observed = []
    failures = []
    tool_requested = threading.Event()
    final = {"type": "message", "id": "msg_probe", "role": "assistant",
             "status": "completed", "content": [{"type": "output_text",
             "text": "OFFLINE_PROBE_OK", "annotations": []}]}

    class Endpoint(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                body = json.loads(raw)
                observed.append({"path": self.path, "stream": body.get("stream"),
                                 "authorization": "Authorization" in self.headers})
                output = final
                if not tool_requested.is_set():
                    # Correlate the client TCP inode to the actual app-server FD.
                    # Observation only: procfs snapshots are NOT an authorization gate.
                    client_address = f"0100007F:{self.client_address[1]:04X}"
                    inodes = {row.split()[9] for row in Path("/proc/net/tcp").read_text().splitlines()[1:]
                              if row.split()[1] == client_address}
                    runtime_sockets = {os.readlink(fd) for fd in Path(f"/proc/{process.pid}/fd").iterdir()}
                    assert any(f"socket:[{inode}]" in runtime_sockets for inode in inodes)
                    assert os.path.samefile(f"/proc/{process.pid}/exe", "/codex")
                    # This transcript is entirely synthetic and lives only in tmpfs.
                    Path("/work/request.json").write_text(json.dumps({
                        "body": raw.decode(), "headers": dict(self.headers)}))
                    names = [tool.get("name") for tool in body.get("tools", [])]
                    command = f"/usr/bin/python3 /probe.py --replay {self.server.server_port}"
                    if "exec_command" in names:
                        name, arguments = "exec_command", {"cmd": command, "max_output_tokens": 1000}
                    elif "shell_command" in names:
                        name, arguments = "shell_command", {"command": command, "timeout_ms": 5000}
                    elif "shell" in names:
                        name, arguments = "shell", {"command": ["/bin/sh", "-c", command], "timeout_ms": 5000}
                    else:
                        raise RuntimeError(f"no supported fixture shell tool: {names}")
                    output = {"type": "function_call", "id": "fc_probe", "call_id": "call_probe",
                              "name": name, "arguments": json.dumps(arguments)}
                    tool_requested.set()
                data = sse(output)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception as error:
                failures.append(str(error))
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    args = ["/codex", "app-server", "--stdio"]
    config = {"model_provider": "probe", "model_providers.probe.name": "offline-probe",
              "model_providers.probe.base_url": f"http://127.0.0.1:{server.server_port}/v1",
              "model_providers.probe.wire_api": "responses",
              "model_providers.probe.requires_openai_auth": False,
              "model_providers.probe.request_max_retries": 0,
              "model_providers.probe.stream_max_retries": 0}
    for key, value in config.items():
        args.extend(["-c", f"{key}={json.dumps(value)}"])
    process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, start_new_session=True)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    completed = False
    tool_output = ""
    reply_seen = False
    turn_status = None
    sibling = None

    def send(method, params, request_id=None):
        frame = {"method": method, "params": params}
        if request_id is not None:
            frame["id"] = request_id
        process.stdin.write((json.dumps(frame) + "\n").encode())
        process.stdin.flush()

    def child_replay(fd=None):
        command = [sys.executable, "/probe.py", "--replay", str(server.server_port)]
        if fd is not None:
            command.append(str(fd))
        return subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                pass_fds=() if fd is None else (fd,))

    def result(child):
        stdout, stderr = child.communicate(timeout=8)
        assert child.returncode == 0, stderr.decode()
        return json.loads(stdout)

    try:
        version = subprocess.check_output(["/codex", "--version"], text=True).strip()
        assert version == "codex-cli 0.153.4", version
        send("initialize", {"clientInfo": {"name": "offline-probe", "version": "1"},
                            "capabilities": {"experimentalApi": True}}, 1)
        buffered = b""
        deadline = time.monotonic() + 35
        while time.monotonic() < deadline and not completed:
            if tool_requested.is_set() and sibling is None:
                sibling = child_replay()
            if not selector.select(0.1):
                continue
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                break
            buffered += chunk
            while b"\n" in buffered:
                line, buffered = buffered.split(b"\n", 1)
                frame = json.loads(line)
                assert "error" not in frame, frame
                if frame.get("id") == 1:
                    send("initialized", {})
                    send("thread/start", {"model": "fixture", "modelProvider": "probe",
                         "cwd": "/work", "ephemeral": True, "approvalPolicy": "never",
                         "sandbox": "danger-full-access"}, 2)
                elif frame.get("id") == 2:
                    send("turn/start", {"threadId": frame["result"]["thread"]["id"],
                         "input": [{"type": "text", "text": "Run the offline fixture tool.",
                                    "text_elements": []}]}, 3)
                if frame.get("method") == "item/completed":
                    item = frame["params"]["item"]
                    if item.get("type") == "commandExecution":
                        tool_output = item.get("aggregatedOutput", "")
                    if item.get("type") == "agentMessage":
                        reply_seen |= "OFFLINE_PROBE_OK" in item.get("text", "")
                if frame.get("method") == "turn/completed":
                    completed = True
                    turn_status = frame["params"]["turn"]["status"]
        assert not failures, failures
        assert completed and reply_seen and turn_status == "completed", (completed, reply_seen, turn_status, tool_output)
        tool = json.loads(tool_output.strip())
        assert tool["accepted"] and tool["pid"] != process.pid, tool
        assert sibling is not None
        sibling_result = result(sibling)
        assert sibling_result["accepted"], sibling_result
        with socket.create_connection(("127.0.0.1", server.server_port), timeout=5) as connection:
            inherited = result(child_replay(connection.fileno()))
        assert inherited["accepted"], inherited
        runtime_pid = process.pid
        process.terminate()
        process.wait(timeout=5)
        after_exit = result(child_replay())
        assert after_exit["accepted"], after_exit
        assert all(item["path"] == "/v1/responses" and item["stream"] is True
                   and not item["authorization"] for item in observed), observed
        print(json.dumps({"verdict": "FAIL_CALLER_ISOLATION", "counterexample_reproduced": True,
                          "version": version, "runtime_pid": runtime_pid,
                          "network": "private namespace; no external routes",
                          "streaming_turn_completed": completed,
                          "actual_codex_tool": tool, "concurrent_sibling": sibling_result,
                          "inherited_tcp_descriptor": inherited, "after_runtime_exit": after_exit,
                          "requests": observed}, indent=2))
    finally:
        # Namespace teardown owns any tool descendants, including on assertion failure.
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        if sibling is not None:
            if sibling.poll() is None:
                sibling.kill()
            sibling.wait(timeout=5)
        selector.close()
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--inside":
        experiment()
    elif len(sys.argv) > 1 and sys.argv[1] == "--replay":
        assert replay(int(sys.argv[2]), int(sys.argv[3]) if len(sys.argv) == 4 else None)
    else:
        if len(sys.argv) != 2:
            raise SystemExit("usage: python3 scripts/probe-codex-endpoint.py /absolute/path/to/codex-0.153.4")
        binary = Path(sys.argv[1]).resolve(strict=True)
        with binary.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != "56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da":
            raise SystemExit("unreviewed Codex executable; this proof pins the measured 0.153.4 binary")
        print(json.dumps({"binary_sha256": digest}), flush=True)
        command = ["bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--clearenv"]
        for root in ("/usr", "/bin", "/lib", "/lib64"):
            if Path(root).exists():
                command.extend(["--ro-bind", root, root])
        command.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
                        "--dir", os.environ["HOME"], "--dir", "/work", "--chdir", "/work",
                        "--setenv", "HOME", os.environ["HOME"], "--setenv", "PATH", "/usr/bin:/bin",
                        "--ro-bind", str(binary), "/codex", "--ro-bind", str(Path(__file__).resolve()),
                        "/probe.py", "/usr/bin/python3", "/probe.py", "--inside",
                        os.readlink("/proc/self/ns/net")])
        # No root/home/run/etc bind and no ambient environment or passed descriptors.
        completed = subprocess.run(command, timeout=55, close_fds=True)
        raise SystemExit(completed.returncode)


if __name__ == "__main__":
    main()
