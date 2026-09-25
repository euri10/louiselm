#!/usr/bin/env python3
"""Offline capture of the Responses request shape stock Codex sends (louiselm-qbr.5.1.3.2.1).

Runs `codex exec` in a bwrap namespace with an empty home, no network and no
credentials, pointed at a loopback fake endpoint configured exactly as the
broker endpoint will be (custom model provider, no OpenAI auth). Prints one
JSON report of the request line, headers and body structure. Body *values* are
reduced to types and sizes except for the policy fields the broker must read
(model, reasoning, stream); the prompt is synthetic either way.

usage: python3 scripts/probe-codex-request-shape.py /absolute/path/to/codex [model] [effort]
"""

import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import threading

POLICY_FIELDS = {"model", "reasoning", "stream", "store", "parallel_tool_calls", "tool_choice"}


def shape(value, depth=0):
    if isinstance(value, dict):
        return {key: shape(item, depth + 1) for key, item in value.items()} if depth < 2 else "object"
    if isinstance(value, list):
        return {"list": len(value), "item_types": sorted({type(item).__name__ for item in value})}
    if isinstance(value, str):
        return f"str[{len(value)}]"
    return type(value).__name__


def metadata_shape(value):
    """Expose structure of JSON-encoded metadata, never its leaf values."""
    if isinstance(value, dict):
        return {key: metadata_shape(item) for key, item in value.items()}
    if isinstance(value, list):
        return {"list": len(value), "items": [metadata_shape(item) for item in value]}
    if isinstance(value, str):
        try:
            nested = json.loads(value)
        except ValueError:
            return shape(value)
        if isinstance(nested, (dict, list)):
            return {"json_string": metadata_shape(nested)}
    return shape(value)


def sse(text):
    item = {"type": "message", "id": "msg_probe", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}]}
    events = [
        {"type": "response.created", "response": {"id": "resp_probe"}},
        {"type": "response.output_item.added", "output_index": 0, "item": item},
        {"type": "response.output_item.done", "output_index": 0, "item": item},
        {"type": "response.completed", "response": {
            "id": "resp_probe", "status": "completed", "output": [item],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}},
    ]
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def experiment(model, effort):
    assert not any(Path(os.environ["HOME"]).iterdir()), "home must start empty"
    assert [row.split()[0] for row in Path("/proc/net/route").read_text().splitlines()[1:]] == []
    observed = []

    class Endpoint(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            record = {
                "request_line": self.requestline,
                "headers": [[name.lower(), value if name.lower() in
                             {"content-type", "accept", "content-encoding", "originator",
                              "openai-beta", "host", "connection", "user-agent"} else f"<{len(value)} bytes>"]
                            for name, value in self.headers.items()],
                "body_bytes": len(raw),
            }
            try:
                body = json.loads(raw)
                record["body_shape"] = shape(body)
                record["metadata_shape"] = metadata_shape(body.get("client_metadata"))
                record["turn_header_shape"] = metadata_shape(self.headers.get("x-codex-turn-metadata"))
                record["policy"] = {key: body[key] for key in POLICY_FIELDS if key in body}
            except (ValueError, UnicodeDecodeError):
                record["body_shape"] = "not-json (compressed or binary)"
            observed.append(record)
            data = sse("OFFLINE_SHAPE_OK")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    config = {"model_provider": "broker", "model": model, "model_reasoning_effort": effort,
              "model_providers.broker.name": "louiselm-broker",
              "model_providers.broker.base_url": f"http://127.0.0.1:{server.server_port}/v1",
              "model_providers.broker.wire_api": "responses",
              "model_providers.broker.requires_openai_auth": False,
              "model_providers.broker.request_max_retries": 0,
              "model_providers.broker.stream_max_retries": 0}
    args = ["/codex", "exec", "--skip-git-repo-check", "--sandbox", "read-only"]
    for key, value in config.items():
        args.extend(["-c", f"{key}={json.dumps(value)}"])
    args.append("Reply with the word ok.")
    result = subprocess.run(args, stdin=subprocess.DEVNULL, capture_output=True, timeout=40)
    server.shutdown()
    print(json.dumps({"codex_exit": result.returncode, "requests": observed}, indent=1))
    return 0 if observed else 1


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--inside":
        raise SystemExit(experiment(sys.argv[2], sys.argv[3]))
    if len(sys.argv) not in (2, 3, 4):
        raise SystemExit(__doc__)
    binary = Path(sys.argv[1]).resolve(strict=True)
    model = sys.argv[2] if len(sys.argv) > 2 else "gpt-5.6-luna"
    effort = sys.argv[3] if len(sys.argv) > 3 else "high"
    with binary.open("rb") as source:
        print(json.dumps({"binary": str(binary),
                          "binary_sha256": hashlib.file_digest(source, "sha256").hexdigest()}), flush=True)
    command = ["bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--clearenv"]
    for root in ("/usr", "/bin", "/lib", "/lib64"):
        if Path(root).exists():
            command.extend(["--ro-bind", root, root])
    home = "/home/probe"
    command.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--dir", home,
                    "--dir", "/work", "--chdir", "/work", "--setenv", "HOME", home,
                    "--setenv", "PATH", "/usr/bin:/bin", "--ro-bind", str(binary), "/codex",
                    "--ro-bind", str(Path(__file__).resolve()), "/probe.py",
                    "/usr/bin/python3", "/probe.py", "--inside", model, effort])
    raise SystemExit(subprocess.run(command, timeout=55, close_fds=True).returncode)


if __name__ == "__main__":
    main()
