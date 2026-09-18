"""Run unmodified Codex against a synthetic endpoint guarded by the BPF probe."""
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
import tempfile
import threading
import time

from guard_probe import Guard
from endpoint_probe import sse


def replay(port):
    try:
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
        connection.request("POST", "/v1/responses", b"{}")
        response = connection.getresponse()
        response.read()
        result = {"accepted": True, "status": response.status}
    except OSError as error:
        if error.errno != 1:
            raise
        result = {"accepted": False, "errno": error.errno}
    finally:
        connection.close()
    print(json.dumps(result), flush=True)


def main(guard_factory=lambda path, _server: Guard(path),
         verdict="STOCK_CODEX_PRIMITIVE_PASS_NOT_VERIFIED"):
    assert sys.argv[1] == "--disposable-vm"
    assert os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    binary = Path("/var/tmp/ow3ok-codex")
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    assert digest == "56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da"
    final = {"type": "message", "id": "msg_probe", "role": "assistant",
             "status": "completed", "content": [{"type": "output_text",
             "text": "OFFLINE_PROBE_OK", "annotations": []}]}
    requests, errors = [], []
    tool_requested = threading.Event()

    class Endpoint(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append({"path": self.path, "authorization": "Authorization" in self.headers})
                output = final
                if not tool_requested.is_set():
                    names = [tool.get("name") for tool in body.get("tools", [])]
                    command = f"python3 /var/tmp/stock_probe.py --replay {self.server.server_port}"
                    if "exec_command" in names:
                        name, arguments = "exec_command", {"cmd": command, "max_output_tokens": 1000}
                    elif "shell_command" in names:
                        name, arguments = "shell_command", {"command": command, "timeout_ms": 5000}
                    elif "shell" in names:
                        name, arguments = "shell", {"command": ["/bin/sh", "-c", command], "timeout_ms": 5000}
                    else:
                        raise RuntimeError(f"no supported fixture shell: {names}")
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
                errors.append(str(error))
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    guard = guard_factory(sys.argv[2], server)
    process = None
    selector = selectors.DefaultSelector()
    try:
        with tempfile.TemporaryDirectory(prefix="ow3ok-stock-", dir="/var/tmp") as home:
            os.chown(home, 65534, 65534)
            args = [str(binary), "app-server", "--stdio"]
            config = {"model_provider": "probe", "model_providers.probe.name": "offline-probe",
                      "model_providers.probe.base_url": f"http://127.0.0.1:{server.server_port}/v1",
                      "model_providers.probe.wire_api": "responses",
                      "model_providers.probe.requires_openai_auth": False,
                      "model_providers.probe.request_max_retries": 0,
                      "model_providers.probe.stream_max_retries": 0}
            for key, value in config.items():
                args.extend(["-c", f"{key}={json.dumps(value)}"])
            guard.set(0, server.server_port)
            process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, cwd=home, start_new_session=True,
                                       user=65534, group=65534, extra_groups=[],
                                       env={"HOME": home, "PATH": "/usr/bin:/bin", "USER": "nobody"})
            pin = os.pidfd_open(process.pid)
            assert os.path.samefile(f"/proc/{process.pid}/exe", binary)
            guard.set(process.pid, server.server_port)
            selector.register(process.stdout, selectors.EVENT_READ)
            def send(method, params, ident=None):
                frame = {"method": method, "params": params}
                if ident is not None:
                    frame["id"] = ident
                process.stdin.write((json.dumps(frame) + "\n").encode())
                process.stdin.flush()
            send("initialize", {"clientInfo": {"name": "offline-probe", "version": "1"},
                                "capabilities": {"experimentalApi": True}}, 1)
            completed, reply_seen, tool_output = False, False, ""
            buffered = b""
            deadline = time.monotonic() + 35
            while time.monotonic() < deadline and not completed:
                if not selector.select(0.1):
                    continue
                chunk = os.read(process.stdout.fileno(), 65536)
                assert chunk, "Codex exited before completion"
                buffered += chunk
                while b"\n" in buffered:
                    line, buffered = buffered.split(b"\n", 1)
                    frame = json.loads(line)
                    assert "error" not in frame, frame
                    if frame.get("id") == 1:
                        send("initialized", {})
                        send("thread/start", {"model": "fixture", "modelProvider": "probe", "cwd": home,
                             "ephemeral": True, "approvalPolicy": "never", "sandbox": "danger-full-access"}, 2)
                    elif frame.get("id") == 2:
                        send("turn/start", {"threadId": frame["result"]["thread"]["id"],
                             "input": [{"type": "text", "text": "Run the offline fixture tool.",
                                        "text_elements": []}]}, 3)
                    elif frame.get("method") == "item/completed":
                        item = frame["params"]["item"]
                        if item.get("type") == "commandExecution":
                            tool_output = item.get("aggregatedOutput", "")
                        if item.get("type") == "agentMessage":
                            reply_seen |= "OFFLINE_PROBE_OK" in item.get("text", "")
                    elif frame.get("method") == "turn/completed":
                        assert frame["params"]["turn"]["status"] == "completed", frame
                        completed = True
            assert not errors, errors
            assert completed and reply_seen, (completed, reply_seen, tool_output)
            tool = json.loads(tool_output.strip())
            assert tool == {"accepted": False, "errno": 1}, tool
            sibling = json.loads(subprocess.check_output(
                [sys.executable, __file__, "--replay", str(server.server_port)],
                user=65534, group=65534, extra_groups=[], timeout=5))
            assert sibling == tool
            process.terminate()
            process.wait(timeout=5)
            guard.set(0, server.server_port)
            after_exit = json.loads(subprocess.check_output(
                [sys.executable, __file__, "--replay", str(server.server_port)],
                user=65534, group=65534, extra_groups=[], timeout=5))
            assert after_exit == tool
            os.close(pin)
            assert len(requests) == 2 and not any(row["authorization"] for row in requests), requests
            print(json.dumps({"verdict": verdict, "binary_sha256": digest,
                              "kernel": os.uname().release, "streaming_turn_completed": completed,
                              "actual_tool": tool, "sibling": sibling, "after_exit": after_exit,
                              "requests": requests}, indent=2))
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        selector.close()
        guard.close()
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--replay":
        replay(int(sys.argv[2]))
    else:
        main()
