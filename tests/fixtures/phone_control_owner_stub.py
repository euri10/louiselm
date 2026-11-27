#!/usr/bin/env python3
"""Isolated journal/verifier double for the native controller integration test."""

import json
import os
import sys
import time

message = json.load(sys.stdin)
if sys.argv[-1] == "verify":
    result = {"v": 1, "type": "verified"}
else:
    hold = os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "hold-completion")
    if "completed" in message.get("outcome", {}):
        open(os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "completion-started"), "w", encoding="utf-8").close()
        deadline = time.monotonic() + 4
        while os.path.exists(hold) and time.monotonic() < deadline:
            time.sleep(0.01)
    outcome = message.get("outcome", {})
    hold_fence = os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "hold-fence")
    if "fenced_no_effect" in outcome and os.path.exists(hold_fence):
        open(os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "fence-started"), "w", encoding="utf-8").close()
        deadline = time.monotonic() + 4
        while os.path.exists(hold_fence) and time.monotonic() < deadline:
            time.sleep(0.01)
    for state in ("completed", "fenced_no_effect"):
        fail_once = os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "fail-once-" + state)
        if state not in outcome or not os.path.exists(fail_once):
            continue
        with open(fail_once, encoding="utf-8") as marker:
            operation = marker.read().strip()
        if operation == message["identity"]["operation_id"]:
            os.unlink(fail_once)
            with open(os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "owner-failures"), "a", encoding="utf-8") as out:
                out.write(message["identity"]["operation_id"] + ":" + state + "\n")
            sys.exit(1)
    with open(os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "owner-reports.jsonl"), "a", encoding="utf-8") as out:
        out.write(json.dumps(message, separators=(",", ":")) + "\n")
    result = {"v": message["v"], "type": "begun" if message["type"] == "begin" else "recorded"}
print(json.dumps(result, separators=(",", ":")))
