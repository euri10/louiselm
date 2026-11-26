"""Crash a dedicated editor and its mock Agent without leaving descendants."""
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

editor, directory, settled, graceful, *replacement = sys.argv[1:]


def terminate(_signum, _frame):
    raise SystemExit(1)


signal.signal(signal.SIGTERM, terminate)
environment = os.environ.copy()
environment["LOUISELM_QUEUE_TEST_DIR"] = directory
environment["LOUISELM_QUEUE_TEST_SETTLED"] = settled
environment["LOUISELM_QUEUE_TEST_GRACEFUL"] = graceful
environment["LOUISELM_QUEUE_TEST_REPLACEMENT"] = replacement[0] if replacement else ""
with tempfile.TemporaryFile() as diagnostics:
    process = subprocess.Popen(
        [editor, "--headless", "--noplugin", "-u", "tests/minimal_init.lua", "-c",
         "luafile tests/fixtures/queue_crash.lua"],
        stdin=subprocess.DEVNULL, stdout=diagnostics, stderr=diagnostics,
        env=environment, start_new_session=True,
    )
    try:
        deadline = time.monotonic() + 20
        while not (Path(directory) / "ready").exists():
            if process.poll() is not None or time.monotonic() >= deadline:
                diagnostics.seek(0)
                sys.stderr.buffer.write(diagnostics.read(8192))
                raise RuntimeError("editor did not prepare the saved queue")
            time.sleep(0.02)
        print("READY", flush=True)
        if graceful == "1":
            if process.wait(timeout=10) != 0:
                raise RuntimeError("editor did not exit cleanly")
        else:
            sys.stdin.readline()
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        if process.poll() is None:
            process.wait(timeout=5)
