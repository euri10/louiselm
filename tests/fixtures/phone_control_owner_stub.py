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
        deadline = time.monotonic() + 4
        while os.path.exists(hold) and time.monotonic() < deadline:
            time.sleep(0.01)
    with open(os.path.join(os.environ["LOUISELM_CAPTURE_STATE_DIR"], "owner-reports.jsonl"), "a", encoding="utf-8") as out:
        out.write(json.dumps(message, separators=(",", ":")) + "\n")
    result = {"v": message["v"], "type": "begun" if message["type"] == "begin" else "recorded"}
print(json.dumps(result, separators=(",", ":")))
