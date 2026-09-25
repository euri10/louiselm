#!/usr/bin/env python3
"""Production authenticated handoff/admission; root only inside disposable KVM."""
import argparse
import array
import errno
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location("loader_gate", Path(__file__).with_name("test-sender-guard-loader.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
BROKER_WORKER = "provider_requests::handoff::production_handoff_broker_worker"


def rights(channel, descriptor):
    channel.sendmsg([b"S"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [descriptor]))])


def take(channel):
    data, ancillary, flags, _ = channel.recvmsg(1, socket.CMSG_SPACE(4), socket.MSG_CMSG_CLOEXEC)
    assert data == b"S" and not flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC) and len(ancillary) == 1
    level, kind, raw = ancillary[0]
    assert (level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS)
    descriptors = array.array("i", raw)
    assert len(descriptors) == 1
    return descriptors[0]


def runtime(fd):
    control = socket.socket(fileno=fd)
    connections = []
    gate.emit({"ready": True})
    for line in sys.stdin:
        action = json.loads(line)
        op = action["op"]
        if op == "connect":
            connections.append(socket.create_connection(("127.0.0.1", action["port"]), timeout=3))
            gate.emit({"connected": True})
        elif op == "export":
            rights(control, connections[-1].fileno())
            gate.emit({"exported": True})
        elif op == "import":
            connections.append(socket.socket(fileno=take(control)))
            gate.emit({"imported": True})
        elif op in ("write", "helper"):
            connection = connections[-1]
            body = json.dumps({"model": "gpt-5.6-luna", "input": [], "reasoning": {"effort": "high"}, "stream": True}, separators=(",", ":"))
            frame = f"POST /v1/responses HTTP/1.1\r\nhost: 127.0.0.1:{action['port']}\r\naccept: text/event-stream\r\ncontent-type: application/json\r\ncontent-length: {len(body)}\r\n\r\n{body}".encode()
            def attempt():
                try:
                    connection.sendall(frame)
                    connection.shutdown(socket.SHUT_WR)
                    return {"sent": True}
                except OSError as error:
                    return {"sent": False, "errno": error.errno}
            if op == "helper":
                read_fd, write_fd = os.pipe()
                child = os.fork()
                if child == 0:
                    os.close(read_fd)
                    os.write(write_fd, json.dumps(attempt()).encode())
                    os._exit(0)
                os.close(write_fd)
                result = json.loads(os.read(read_fd, 4096))
                os.close(read_fd)
                assert os.waitpid(child, 0)[1] == 0
            else:
                result = attempt()
            gate.emit(result)
        elif op == "read":
            response = b""
            while chunk := connections[-1].recv(4096):
                response += chunk
            assert response.startswith(b"HTTP/1.1 200 OK\r\n") and b"event: done" in response
            gate.emit({"response": True})
        else:
            raise AssertionError(op)


def executable(path, pattern):
    path = path.resolve()
    if path.is_dir():
        candidates = [candidate for candidate in path.glob(pattern) if candidate.is_file() and os.access(candidate, os.X_OK)]
        assert len(candidates) == 1, ("ambiguous test executable", candidates)
        return str(candidates[0])
    return str(path)


def scenario(library, broker_tests, variant):
    children, namespaces, controls, maps, observers = [], [], [], [], []
    def spawn(command, uid=None, namespace=None, pass_fds=()):
        if uid is not None:
            command = ["setpriv", f"--reuid={uid}", f"--regid={uid}", "--clear-groups", "--", *command]
        if namespace:
            command = ["ip", "netns", "exec", namespace, *command]
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, pass_fds=pass_fds)
        children.append(process)
        return process
    with tempfile.TemporaryDirectory(prefix="sender-handoff-", dir="/var/tmp") as directory:
        root = Path(directory)
        os.chown(root, gate.BROKER_UID, gate.BROKER_UID)
        try:
            upstream = socket.socket()
            observers.append(upstream)
            upstream.bind(("127.0.0.1", 0))
            upstream.listen(4)
            upstream.settimeout(5)
            destination = f"127.0.0.1:{upstream.getsockname()[1]}"
            peer = spawn([broker_tests, "--exact", BROKER_WORKER, "--ignored", "--nocapture"], gate.BROKER_UID)
            broker_path = str(root / "control.sock")
            gate.send(peer, {"socket": broker_path, "destination": destination})
            ready = gate.receive(peer, True)
            assert ready["ready"]
            owners, runtimes, ports = [], [], []
            for index in range(1 if variant == "invalid" else 2):
                namespace = f"lh-{os.getpid()}-{index}"
                subprocess.run(["ip", "netns", "add", namespace], check=True)
                namespaces.append(namespace)
                subprocess.run(["ip", "netns", "exec", namespace, "ip", "link", "set", "lo", "up"], check=True)
                routes = subprocess.check_output(["ip", "netns", "exec", namespace, "ip", "route", "show"], text=True)
                assert not routes.strip(), "Session has an ambient route"
                control, child_control = socket.socketpair()
                controls.append(control)
                agent = spawn([sys.executable, __file__, "--runtime", str(child_control.fileno())], gate.UID + index, namespace, (child_control.fileno(),))
                child_control.close()
                assert gate.receive(agent)["ready"]
                os.kill(agent.pid, signal.SIGSTOP)
                assert os.WIFSTOPPED(os.waitpid(agent.pid, os.WUNTRACED)[1])
                owner = spawn([library, "--exact", gate.WORKER, "--ignored", "--nocapture"])
                gate.send(peer, {"op": "launch"})
                gate.send(owner, {"broker": broker_path, "session": f"session-{index}", "runtime": agent.pid, "uid": gate.UID + index,
                    "executable": sys.executable, "launch_request": ready["requests"][index], "invalid_handoff": variant == "invalid"})
                assert gate.receive(peer, True)["launched"]
                gate.send(peer, {"op": "enroll", "index": index, "refused": variant == "invalid"})
                enrolled = gate.receive(owner, True)
                maps.extend(enrolled["maps"])
                if variant == "invalid":
                    assert enrolled["refused"] and gate.receive(peer, True)["refused"]
                    assert owner.wait(5) == 0, owner.stderr.read()[-4000:]
                    break
                assert enrolled["ready"] and gate.receive(peer, True)["enrolled"]
                port = enrolled["port"]
                os.kill(agent.pid, signal.SIGCONT)
                assert gate.request(agent, {"op": "connect", "port": port})["connected"]
                assert gate.request(agent, {"op": "write", "port": port}) == {"sent": False, "errno": errno.EPERM}
                assert gate.request(owner, {"op": "activate"}, True)["activated"]
                assert gate.request(agent, {"op": "helper", "port": port}) == {"sent": False, "errno": errno.EPERM}
                assert gate.request(agent, {"op": "write", "port": port})["sent"]
                gate.send(owner, {"op": "status"})
                gate.send(peer, {"op": "serve", "index": index})
                assert gate.receive(owner, True)["status"]
                served = gate.receive(peer, True)
                assert served["served"] and served["calls"] == index + 1 and served["spent"] == index + 1
                assert gate.request(agent, {"op": "read"})["response"]
                gate.send(owner, {"op": "transfer", "address": destination})
                assert gate.receive(owner, True)["handoff"]
                gate.send(peer, {"op": "upstream", "index": index})
                assert gate.receive(peer, True)["taken"] == index
                assert gate.receive(owner, True)["connected"]
                observed, _ = upstream.accept()
                observed.settimeout(3)
                observers.append(observed)
                assert gate.request(peer, {"op": "send", "index": index}, True)["sent"]
                assert observed.recv(4096) == b"synthetic-request\n"
                owners.append(owner)
                runtimes.append(agent)
                ports.append(port)
            if variant != "invalid":
                # Transfer a Session-0 connected socket into the enrolled Session-1
                # process. Its own valid grant cannot authorize this namespace/socket.
                assert gate.request(runtimes[0], {"op": "connect", "port": ports[0]})["connected"]
                assert gate.request(runtimes[0], {"op": "export"})["exported"]
                exported = take(controls[0])
                rights(controls[1], exported)
                os.close(exported)
                assert gate.request(runtimes[1], {"op": "import"})["imported"]
                assert gate.request(runtimes[1], {"op": "write", "port": ports[0]}) == {"sent": False, "errno": errno.EPERM}
                if variant == "partial":
                    # The receiver refuses an inexact request id while alive.
                    # Channel closure alone is not kernel revocation or cleanup.
                    gate.send(owners[0], {"op": "transfer", "address": destination, "refused": True})
                    assert gate.receive(owners[0], True)["handoff"]
                    assert gate.request(peer, {"op": "upstream", "index": 0, "refused": True}, True)["refused"]
                    assert gate.receive(owners[0], True)["refused"]
                    assert gate.request(runtimes[0], {"op": "write", "port": ports[0]}) == {"sent": False, "errno": errno.EPERM}
                if variant in ("broker-crash", "partial"):
                    peer.kill()
                    peer.wait(5)
                    for owner in owners:
                        assert gate.request(owner, {"op": "lost"}, True)["lost"]
                else:
                    assert gate.request(owners[0], {"op": "revoke"}, True)["revoked"]
                    assert gate.request(runtimes[0], {"op": "write", "port": ports[0]}) == {"sent": False, "errno": errno.EPERM}
                    assert not gate.request(peer, {"op": "send", "index": 0}, True)["sent"]
                    assert gate.request(peer, {"op": "send", "index": 1}, True)["sent"]
                    assert observers[2].recv(4096) == b"synthetic-request\n"
                for index, owner in enumerate(owners):
                    runtimes[index].kill()
                    runtimes[index].wait(5)
                    if variant not in ("broker-crash", "partial"):
                        gate.send(peer, {"op": "close", "index": index})
                    gate.send(owner, {"op": "close"})
                    if variant not in ("broker-crash", "partial"):
                        assert gate.receive(peer, True)["closed"]
                    assert owner.wait(5) == 0, owner.stderr.read()[-4000:]
            if peer.poll() is None:
                assert gate.request(peer, {"op": "halt"}, True)["halted"]
                assert peer.wait(5) == 0
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    process.kill()
                process.wait(5)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()
            for connection in controls + observers:
                connection.close()
            for namespace in namespaces:
                subprocess.run(["ip", "netns", "delete", namespace], check=True)
        gate.assert_maps_released(maps)
    print(f"production handoff: {variant} passed", flush=True)


def main():
    if sys.argv[1:2] == ["--runtime"]:
        runtime(int(sys.argv[2]))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library_tests", type=Path)
    parser.add_argument("broker_tests", type=Path)
    args = parser.parse_args()
    assert os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    assert "bpf" in Path("/sys/kernel/security/lsm").read_text().split(",")
    library = executable(args.library_tests, "louiselm_skills-*")
    broker_tests = executable(args.broker_tests, "broker-*")
    for variant in ("normal", "invalid", "broker-crash", "partial"):
        scenario(library, broker_tests, variant)
    print("PRODUCTION_HANDOFF_COMPONENT_PASS_NOT_VERIFIED", flush=True)


if __name__ == "__main__":
    main()
