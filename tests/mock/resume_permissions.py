"""Offline ACP peer for the negotiated Resume permissions extension, not a vendor capture."""

import json
import sys

KEY = "io.github.euri10.louiselm.resumePermissions"
MODES = [
    {"id": "fixture-read-only", "name": "Read Only", "description": "No file changes"},
    {"id": "fixture-full-access", "name": "Full Access", "description": "No approval prompts"},
]


def send(message):
    print(json.dumps({"jsonrpc": "2.0", **message}), flush=True)


# Only synthetic test requests reach this disposable ledger. No Provider or
# filesystem tools exist in this peer; each restarted process shares the ledger.
with open(sys.argv[1], "a", encoding="utf-8", buffering=1) as ledger:
    for line in sys.stdin:
        request = json.loads(line)
        ledger.write(json.dumps(request) + "\n")
        if "id" not in request:
            continue
        method = request["method"]
        params = request.get("params", {})
        if method == "initialize":
            send({"id": request["id"], "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": True, "promptCapabilities": {"embeddedContext": True}},
            }})
        elif method == "session/load":
            choice = params.get("_meta", {}).get(KEY)
            if choice not in [{"version": 1, "modeId": mode["id"]} for mode in MODES]:
                send({"id": request["id"], "error": {
                    "code": -32600,
                    "message": "Saved permissions require an explicit replacement",
                    "data": {"reason": "resume_permissions_unavailable", KEY: {"version": 1, "modes": MODES}},
                }})
                continue
            send({"method": "session/update", "params": {
                "sessionId": params["sessionId"],
                "update": {"sessionUpdate": "agent_message_chunk", "content": {
                    "type": "text", "text": "replayed fixture history",
                }},
            }})
            send({"id": request["id"], "result": {"_meta": {KEY: choice}}})
        elif method == "session/prompt":
            send({"id": request["id"], "result": {"stopReason": "end_turn"}})
        else:
            send({"id": request["id"], "error": {"code": -32601, "message": "method not found"}})
