#!/usr/bin/env python3
"""Opt-in offline authentication feasibility probe, louiselm-qbr.5.1.3.4.

Exit zero reproduces a custody counterexample; it does NOT prove subscription
access. Only synthetic tokens enter a private, externally disconnected namespace.
"""

import base64
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import threading
import time


def token(generation):
    def encode(value):
        return base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")

    return ".".join([encode({"alg": "none"}), encode({
        "sub": "synthetic-offline-only", "email": "fixture@example.invalid",
        "exp": 4102444800, "generation": generation,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "offline-account", "chatgpt_plan_type": "plus"},
    }), "synthetic-signature"])


class Rpc:
    def __init__(self, process):
        self.process = process
        self.selector = selectors.DefaultSelector()
        self.selector.register(process.stdout, selectors.EVENT_READ)
        self.buffer = b""
        self.sequence = 0
        self.notifications = []
        self.refreshes = 0

    def send(self, frame):
        self.process.stdin.write((json.dumps(frame) + "\n").encode())
        self.process.stdin.flush()

    def receive(self, deadline):
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            assert remaining > 0 and self.selector.select(remaining), "RPC timeout"
            chunk = os.read(self.process.stdout.fileno(), 65536)
            assert chunk, "app-server exited before its response"
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line)

    def dispatch(self, frame):
        if frame.get("method") == "account/chatgptAuthTokens/refresh":
            assert frame["params"]["reason"] == "unauthorized"
            assert frame["params"]["previousAccountId"] == "offline-account"
            self.refreshes += 1
            self.send({"id": frame["id"], "result": {
                "accessToken": token(2), "chatgptAccountId": "offline-account",
                "chatgptPlanType": "plus"}})
        else:
            assert "id" not in frame, "unexpected server request"
            self.notifications.append(frame)

    def call(self, method, params=None, *, error=False):
        self.sequence += 1
        self.send({"id": self.sequence, "method": method, "params": params})
        deadline = time.monotonic() + 12
        while True:
            frame = self.receive(deadline)
            if frame.get("id") == self.sequence and "method" not in frame:
                assert ("error" in frame) == error, "unexpected RPC result for " + method
                return frame.get("error") if error else frame["result"]
            self.dispatch(frame)


@contextmanager
def runtime(port):
    args = ["/codex", "app-server", "--stdio"]
    for key, value in {
        "cli_auth_credentials_store": "file",
        "features.apps": False,
        "model_provider": "probe", "model_providers.probe.name": "offline",
        "model_providers.probe.base_url": f"http://127.0.0.1:{port}/v1",
        "model_providers.probe.wire_api": "responses",
        "model_providers.probe.requires_openai_auth": True,
        "model_providers.probe.request_max_retries": 0,
        "model_providers.probe.stream_max_retries": 0,
    }.items():
        args.extend(["-c", key + "=" + json.dumps(value)])
    process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, start_new_session=True)
    rpc = Rpc(process)
    try:
        rpc.call("initialize", {"clientInfo": {"name": "offline-auth-probe", "version": "1"},
                                "capabilities": {"experimentalApi": True}})
        rpc.send({"method": "initialized", "params": {}})
        yield rpc
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        process.stdin.close()
        process.stdout.close()
        rpc.selector.close()


def experiment():
    assert os.readlink("/proc/self/ns/net") != sys.argv[2], "network isolation missing"
    assert not list(Path(os.environ["HOME"]).iterdir()), "home must start empty"
    assert not Path("/proc/net/route").read_text().splitlines()[1:], "external route present"
    version = subprocess.check_output(["/codex", "--version"], text=True).strip()
    assert version == "codex-cli 0.153.4", "unexpected executable version"
    requests = []
    failures = []

    class Endpoint(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                generation = len(requests) + 1
                assert self.path == "/v1/responses" and body["stream"] is True
                assert generation <= 2, "unexpected retry"
                assert self.headers.get("Authorization") == "Bearer " + token(generation)
                requests.append({"path": self.path, "synthetic_bearer_generation": generation})
                if generation == 1:
                    self.send_response(401)
                    data = b'{"error":{"message":"synthetic expired token"}}'
                    content_type = "application/json"
                else:
                    self.send_response(200)
                    output = {"type": "message", "id": "msg_probe", "role": "assistant",
                              "status": "completed", "content": [{"type": "output_text",
                              "text": "OFFLINE_AUTH_OK", "annotations": []}]}
                    event = {"type": "response.completed", "response": {
                        "id": "resp_probe", "status": "completed", "output": [output],
                        "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}}
                    data = ("event: response.completed\ndata: " + json.dumps(event) + "\n\n").encode()
                    content_type = "text/event-stream"
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception:
                failures.append("unexpected synthetic HTTP request")
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with runtime(server.server_port) as rpc:
            assert rpc.call("account/read", {"refreshToken": False})["account"] is None
            login = rpc.call("account/login/start", {"type": "chatgpt"})
            assert login["type"] == "chatgpt" and login["loginId"] and login["authUrl"]
            rpc.call("account/login/cancel", {"loginId": login["loginId"]})
            assert rpc.call("account/read", {"refreshToken": False})["account"] is None
            rpc.call("account/login/start", {"type": "chatgptAuthTokens",
                     "accessToken": "", "chatgptAccountId": "offline-account"}, error=True)
            rpc.call("account/login/start", {"type": "chatgptAuthTokens",
                     "accessToken": token(1), "chatgptAccountId": "offline-account",
                     "chatgptPlanType": "plus"})
            account = rpc.call("account/read", {"refreshToken": True})
            assert set(account) == {"account", "requiresOpenaiAuth"}
            assert set(account["account"]) == {"type", "email", "planType"}
            assert account["account"]["type"] == "chatgpt" and rpc.refreshes == 0
            legacy = rpc.call("getAuthStatus", {"includeToken": True, "refreshToken": False})
            assert legacy["authMethod"] == "chatgptAuthTokens" and legacy["authToken"] == token(1)
            thread = rpc.call("thread/start", {"model": "fixture", "modelProvider": "probe",
                              "cwd": "/work", "ephemeral": True,
                              "approvalPolicy": "never", "sandbox": "read-only"})
            rpc.call("turn/start", {"threadId": thread["thread"]["id"],
                     "input": [{"type": "text", "text": "Offline authentication probe.",
                                "text_elements": []}]})
            deadline = time.monotonic() + 25
            while not any(frame.get("method") == "turn/completed" for frame in rpc.notifications):
                try:
                    rpc.dispatch(rpc.receive(deadline))
                except AssertionError:
                    print(json.dumps({"requests": requests, "refreshes": rpc.refreshes,
                          "failures": failures,
                          "events": [frame.get("method") for frame in rpc.notifications]}),
                          file=sys.stderr)
                    raise
            turn = next(frame["params"]["turn"] for frame in rpc.notifications
                        if frame.get("method") == "turn/completed")
            assert turn["status"] == "completed", "synthetic turn failed"
            assert not failures and len(requests) == 2 and rpc.refreshes == 1
        # Restart with the SAME disposable home: external tokens were not persisted.
        with runtime(server.server_port) as rpc:
            assert rpc.call("account/read", {"refreshToken": False})["account"] is None
            rpc.call("account/logout")
            assert rpc.call("account/read", {"refreshToken": False})["account"] is None
        # Synthetic managed-cache fixture, NOT a login or an imported credential.
        auth = Path(os.environ["HOME"]) / ".codex" / "auth.json"
        auth.write_text(json.dumps({"auth_mode": "chatgpt", "OPENAI_API_KEY": None,
            "tokens": {"id_token": token(1), "access_token": token(1),
                       "refresh_token": "synthetic-offline-refresh", "account_id": "offline-account"},
            "last_refresh": datetime.now(timezone.utc).isoformat()}))
        auth.chmod(0o600)
        with runtime(server.server_port) as rpc:
            legacy = rpc.call("getAuthStatus", {"includeToken": True, "refreshToken": False})
            assert legacy["authMethod"] == "chatgpt" and legacy["authToken"] == token(1)
            assert rpc.call("account/read", {"refreshToken": False})["account"]["type"] == "chatgpt"
            rpc.call("account/logout")
            assert rpc.call("account/read", {"refreshToken": False})["account"] is None
        print(json.dumps({"verdict": "BLOCKED_SUBSCRIPTION_BRIDGE", "version": version,
              "counterexample_reproduced": True, "subscription_acceptance": "NOT_TESTED",
              "external_token_custody": "FAIL: app-server receives reusable bearer",
              "managed_login": "started and cancelled; no upstream login",
              "empty_external_token": "rejected", "account_read": "metadata only",
              "legacy_getAuthStatus": "exports external AND synthetic managed-cache access tokens",
              "managed_cache": "synthetic fixture loaded after restart; logout clears account",
              "external_forced_refresh": "ignored", "401_refresh": "host callback then retry",
              "external_restart": "unauthenticated", "requests": requests}, indent=2))
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--inside":
        experiment()
        return
    if len(sys.argv) != 2:
        raise SystemExit("usage: python3 scripts/probe-codex-auth.py /absolute/path/to/codex-0.153.4")
    binary = Path(sys.argv[1]).resolve(strict=True)
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    if digest != "56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da":
        raise SystemExit("unreviewed Codex executable; expected measured 0.153.4 binary")
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
    # The operator's home path is an EMPTY namespace directory, never a mount.
    # No /etc, /run, host home, credential store, repository or inherited descriptors.
    raise SystemExit(subprocess.run(command, timeout=75, close_fds=True).returncode)


if __name__ == "__main__":
    main()
