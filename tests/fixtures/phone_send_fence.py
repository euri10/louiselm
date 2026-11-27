#!/usr/bin/env python3
"""Pause the real journal's begun response while the desktop Session advances."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

binary, *args = sys.argv[1:]
if args != ["phone-control", "owner-journal"]:
    os.execv(binary, [binary, *args])

request = sys.stdin.buffer.read()
result = subprocess.run([binary, *args], input=request, capture_output=True, check=False)
if json.loads(request)["type"] == "begin" and result.returncode == 0:
    assert json.loads(result.stdout)["type"] == "begun"
    state = Path(os.environ["LOUISELM_CAPTURE_STATE_DIR"])
    (state / "fence-begun").touch()
    deadline = time.monotonic() + 4
    while not (state / "fence-release").exists():
        if time.monotonic() >= deadline:
            sys.exit(1)
        time.sleep(0.01)
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
