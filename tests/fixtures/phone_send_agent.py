#!/usr/bin/env python3
"""Record the disposable Agent's ACP input and reap it when the editor stops."""

import os
import signal
import subprocess
import sys

stop_requested = False


def request_stop(_signal, _frame):
    global stop_requested
    stop_requested = True


def interrupt(_signal, _frame):
    raise SystemExit(0)


signal.signal(signal.SIGTERM, request_stop)
agent = None
try:
    with open(os.environ["LOUISELM_FIXTURE_WIRE"], "wb") as wire:
        agent = subprocess.Popen(sys.argv[1:], stdin=subprocess.PIPE, bufsize=0)
        signal.signal(signal.SIGTERM, interrupt)
        if not stop_requested:
            for line in sys.stdin.buffer:
                wire.write(line)
                wire.flush()
                pending = memoryview(line)
                while pending:
                    written = agent.stdin.write(pending)
                    if not written:
                        raise BrokenPipeError("mock Agent closed its input")
                    pending = pending[written:]
finally:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    if agent is not None:
        try:
            agent.stdin.close()
        except BrokenPipeError:
            # The child already closed its input; it still needs to be reaped.
            pass
        try:
            agent.terminate()
        except ProcessLookupError:
            # An exited child still needs wait() below.
            pass
        try:
            agent.wait(timeout=1)
        except subprocess.TimeoutExpired:
            agent.kill()
            agent.wait()
