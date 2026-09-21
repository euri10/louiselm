"""Compose the real sendmmsg hook counterexample with the Rust admission fixture.

Run only as root inside a disposable restricted KVM guest. All bytes are
synthetic; the Rust executable must be the broker integration-test target.
"""
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import time

from guard_probe import Guard, receive, transmit


def sender(port, payload):
    print(json.dumps({"ready": os.getpid()}), flush=True)
    assert sys.stdin.readline() == "send\n"
    with socket.create_connection(("127.0.0.1", port), timeout=5) as connection:
        print(json.dumps(transmit(connection, "sendmmsg", payload.encode())), flush=True)


def main():
    assert len(sys.argv) == 4 and sys.argv[1] == "--disposable-vm"
    assert os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    assert "bpf" in Path("/sys/kernel/security/lsm").read_text().split(",")
    broker = subprocess.Popen(
        [sys.argv[3], "--ignored", "--exact", "buffered_requests::kernel_sendmmsg", "--nocapture"],
        env={"PATH": "/usr/bin:/bin", "LOUISELM_REQUEST_PROOF_VM": "1"},
        stdout=subprocess.PIPE, text=True, bufsize=1,
    )
    guard = process = pin = None
    try:
        # Read bytes directly: TextIO can buffer several lines beyond select's
        # readiness observation. Every wait and the complete handshake are bounded.
        raw = b""
        deadline = time.monotonic() + 10
        while b"REQUEST_PROOF_ENDPOINT=" not in raw or not raw.endswith(b"\n"):
            remaining = deadline - time.monotonic()
            assert remaining > 0 and select.select([broker.stdout], [], [], remaining)[0]
            chunk = os.read(broker.stdout.fileno(), 4096)
            assert chunk, "broker exited before endpoint"
            raw += chunk
        line = next(line for line in raw.decode().splitlines() if line.startswith("REQUEST_PROOF_ENDPOINT="))
        endpoint = json.loads(line.split("=", 1)[1])
        guard = Guard(sys.argv[2])
        process = subprocess.Popen(
            [sys.executable, __file__, "--sender", str(endpoint["port"]), endpoint["payload"]],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
            user=65534, group=65534, extra_groups=[],
        )
        assert receive(process)["ready"] == process.pid
        pin = os.pidfd_open(process.pid)
        guard.set(process.pid, endpoint["port"])
        process.stdin.write("send\n")
        process.stdin.flush()
        result = receive(process)
        assert result == {"accepted": True, "requests": 2}, result
        assert process.wait(timeout=5) == 0
        output = broker.communicate(timeout=10)[0]
        assert broker.returncode == 0, output
        assert "REQUEST_PROOF_ADMISSIONS=2" in output, output
        assert guard.checks() == 1, guard.checks()
        print(json.dumps({"kernel": os.uname().release, "send_hook_checks": 1,
                          "broker_admissions": 2, "spent_fixture_reservations": 2,
                          "verdict": "REQUEST_BOUNDARY_COMPONENT_PASS_NOT_VERIFIED"}, indent=2))
    finally:
        # Close the endpoint before releasing the process-owned experimental guard.
        for child in (broker, process):
            if child is not None:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5)
        if pin is not None:
            os.close(pin)
        if guard is not None:
            guard.close()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--sender":
        sender(int(sys.argv[2]), sys.argv[3])
    else:
        main()
