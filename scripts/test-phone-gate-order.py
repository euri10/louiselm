#!/usr/bin/env python3
"""Exercise the native phone gate's real process/cutoff ordering on Linux."""

import json
import array
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
DELAY_SECONDS = 3.5


def receiver(directory):
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(Path(directory) / "receiver.sock"))
    listener.listen(1)
    print("READY", flush=True)
    connection, _ = listener.accept()
    channel = socket.socket(fileno=int(os.environ["LOUISELM_GATE_FD_CHANNEL"]))
    channel.sendmsg([b"D"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [connection.fileno()]))])
    data = b""
    while not data.endswith(b"\n"):
        chunk = connection.recv(4096)
        if not chunk:
            raise RuntimeError("owner disconnected before its gate")
        data += chunk
    print("GATE " + data.decode().strip(), flush=True)
    if sys.stdin.readline().strip() == "cut":
        connection.shutdown(socket.SHUT_RDWR)
        with (Path(directory) / "revoke-intent").open("wb") as intent:
            intent.write(b"disabled\n")
            intent.flush()
            os.fsync(intent.fileno())
        parent = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
        print("CUT", flush=True)
        sys.stdin.readline()


def line(process, timeout):
    readable, _, _ = select.select([process.stdout], [], [], timeout)
    if not readable:
        raise RuntimeError("disposable receiver timed out")
    value = process.stdout.readline()
    if not value:
        raise RuntimeError("disposable receiver exited unexpectedly")
    return value.strip()


def scenario(kind):
    with tempfile.TemporaryDirectory(prefix="louiselm-phone-gate-") as directory:
        channel, child_channel = socket.socketpair()
        server_env = os.environ.copy()
        server_env["LOUISELM_GATE_FD_CHANNEL"] = str(child_channel.fileno())
        server = subprocess.Popen(
            [sys.executable, __file__, "receiver", directory],
            env=server_env,
            pass_fds=(child_channel.fileno(),),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        child_channel.close()
        owner = None
        duplicate = None
        try:
            assert line(server, 10) == "READY"
            client = socket.socket(socket.AF_UNIX)
            client.connect(str(Path(directory) / "receiver.sock"))
            client_fd = client.fileno()
            clock_fd = os.open("/proc/uptime", os.O_RDONLY) if kind == "late" else None
            traced_fd = clock_fd if clock_fd is not None else client_fd
            syscall = "pread64" if kind == "late" else "write"
            delay = 6.5 if kind == "late" else DELAY_SECONDS
            environment = os.environ.copy()
            environment.update({
                "LOUISELM_GATE_ROOT": str(ROOT),
                "LOUISELM_GATE_TMP": directory,
                "LOUISELM_GATE_RECEIVER_PID": str(server.pid),
                "LOUISELM_GATE_SOCKET_FD": str(client_fd),
                "XDG_STATE_HOME": directory,
            })
            environment.pop("LOUISELM_CAPTURE_STATE_DIR", None)
            if clock_fd is not None:
                environment["LOUISELM_GATE_CLOCK_FD"] = str(clock_fd)
            trace = Path(directory) / "trace.log"
            with (Path(directory) / "owner.log").open("w") as output:
                owner = subprocess.Popen(
                    [shutil.which("strace"), "-s", "512", "-o", str(trace), "-e", f"trace={syscall}",
                     "-e", f"trace-fds={traced_fd}",
                     "-e", f"inject={syscall}:delay_exit={int(delay * 1_000_000)}",
                     shutil.which("nvim"), "--headless", "--noplugin", "-u", "NONE",
                     "-l", str(ROOT / "tests/fixtures/phone_gate_process.lua")],
                    cwd=ROOT, env=environment, stdout=output, stderr=subprocess.STDOUT,
                    start_new_session=True, pass_fds=(client_fd,) if clock_fd is None else (client_fd, clock_fd),
                )
                client.close()
                if clock_fd is not None:
                    os.close(clock_fd)
                gate_line = line(server, 60)
                received_at = time.monotonic()
                prefix, frame = gate_line.split(" ", 1)
                assert prefix == "GATE"
                _, ancillary, _, _ = channel.recvmsg(1, socket.CMSG_SPACE(array.array("i").itemsize))
                descriptors = array.array("i")
                descriptors.frombytes(ancillary[0][2][:descriptors.itemsize])
                duplicate = socket.socket(fileno=descriptors[0])
                witness = json.loads(frame)
                assert witness == {"v": 2, "type": "gate_use", "controller_id": "01" * 16,
                                   "operation_id": "02" * 16, "gate_id": "03" * 16}
                if kind == "late":
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline:
                        if "(DELAYED)" in trace.read_text(errors="replace"):
                            break
                        time.sleep(0.01)
                    else:
                        raise RuntimeError("the final clock sample did not reach its delayed syscall")
                if kind in ("cut", "late"):
                    server.stdin.write("cut\n")
                    server.stdin.flush()
                    assert line(server, 2) == "CUT"
                    assert (Path(directory) / "revoke-intent").read_bytes() == b"disabled\n"
                if kind in ("death", "late"):
                    server.kill()
                    server.wait(timeout=5)
                    assert duplicate.fileno() >= 0  # The original peer remains open after receiver death.
                assert time.monotonic() - received_at < delay - 0.5
                owner.wait(timeout=45)
                output.flush()
            if owner.returncode != 0:
                result_path = Path(directory) / "result.json"
                result_text = result_path.read_text(errors="replace") if result_path.exists() else "missing"
                trace_tail = "\n".join(trace.read_text(errors="replace").splitlines()[-8:])
                raise RuntimeError(f"owner exit {owner.returncode}; result={result_text}; "
                                   f"log={(Path(directory) / 'owner.log').read_text(errors='replace')}; trace={trace_tail}")
            result = json.loads((Path(directory) / "result.json").read_text())
            assert result["admitted"] is (kind != "death"), result
            assert result["current_turn"] == (0 if kind == "death" else 1), result
            if kind == "late":
                assert time.monotonic() - received_at > 5
            lines = trace.read_text(errors="replace").splitlines()
            needle = "pread64(" if kind == "late" else "gate_use"
            assert any(needle in item and "(DELAYED)" in item for item in lines), "expected syscall was not delayed"
            print(f"{kind}: gate ordering verified")
        finally:
            if owner is not None and owner.poll() is None:
                os.killpg(owner.pid, signal.SIGKILL)
                owner.wait(timeout=5)
            if server.poll() is None:
                server.kill()
                server.wait(timeout=5)
            if duplicate is not None:
                duplicate.close()
            channel.close()


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "receiver":
        receiver(sys.argv[2])
    else:
        if shutil.which("strace") is None or shutil.which("nvim") is None:
            raise SystemExit("strace and nvim are required")
        scenario("cut")
        scenario("death")
        scenario("late")
