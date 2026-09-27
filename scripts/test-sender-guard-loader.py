#!/usr/bin/env python3
"""Production Rust loader/ownership gate; root inside a disposable KVM only."""

import argparse
import array
import ctypes
import errno
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import tempfile
import time

UID = 4020000
BROKER_UID = 4020010
WORKER = "launch_supervisor::sender_guard::tests::production_loader_worker"


def emit(value):
    print(json.dumps(value), flush=True)


def send(process, value):
    process.stdin.write(json.dumps(value) + "\n")
    process.stdin.flush()


def receive(process, rust=False):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        buffered = getattr(process, "_guard_output", "")
        if "\n" not in buffered:
            if not select.select([process.stdout], [], [], 1)[0]:
                continue
            data = os.read(process.stdout.fileno(), 65536)
            assert data, ("fixture EOF", process.stderr.read()[-4000:])
            process._guard_output = buffered + data.decode()
            continue
        line, process._guard_output = buffered.split("\n", 1)
        if rust:
            if "GUARD_FIXTURE " not in line:
                continue
            line = line.split("GUARD_FIXTURE ", 1)[1]
        return json.loads(line)
    raise AssertionError("fixture response deadline")


def request(process, value, rust=False):
    send(process, value)
    return receive(process, rust)


def broker(path):
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    listener.bind(path)
    listener.listen(8)
    listener.settimeout(5)
    channels, sockets, leases = [], [], []
    endpoints = {}
    emit({"ready": True})
    for line in sys.stdin:
        action = json.loads(line)
        op = action["op"]
        if op == "enrollment":
            if action.get("new", True):
                channel, _ = listener.accept()
                channel.settimeout(5)
                channels.append(channel)
            channel = channels[action.get("channel", -1)]
            peer = array.array("i", channel.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            assert peer[0] == action["owner"] and peer[1] == 0
            raw, ancillary, flags, _ = channel.recvmsg(65536, socket.CMSG_SPACE(12))
            assert not flags and len(ancillary) == 1
            level, kind, rights = ancillary[0]
            assert (level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS)
            descriptors = array.array("i", rights)
            assert len(descriptors) == 3
            endpoint = socket.socket(fileno=descriptors[0])
            evidence = json.loads(raw)
            result = evidence["result"]
            assert result["kind"] == "sender_guard_enrolled", result
            result = result["enrollment"]
            assert result["broker_pid"] == os.getpid()
            assert result["scope"]["session_id"] == action["session"]
            assert result["scope"]["revision"] == action.get("revision", 1)
            assert os.fstat(descriptors[1]).st_ino == result["guard_id"]
            assert os.fstat(descriptors[2]).st_ino == result["network_id"]
            assert int.from_bytes(endpoint.getsockopt(socket.SOL_SOCKET, 57, 8), sys.byteorder) == result["listener_cookie"]
            index = action.get("channel", len(channels) - 1)
            assert index not in endpoints
            endpoints[index] = (endpoint, descriptors[1:])
            evidence["result"]["kind"] = "sender_guard_accepted"
            channel.send(json.dumps(evidence, separators=(",", ":")).encode())
            emit({"enrolled": True})
        elif op == "close_endpoint":
            index = action["channel"]
            channel = channels[index]
            evidence = json.loads(channel.recv(65536))
            assert evidence["result"]["kind"] == "sender_guard_closing"
            endpoint, references = endpoints.pop(index)
            endpoint.close()
            for fd in references:
                os.close(fd)
            evidence["result"]["kind"] = "sender_guard_closed"
            channel.send(json.dumps(evidence, separators=(",", ":")).encode())
            emit({"endpoint_closed": True})
        elif op == "take":
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as channel:
                channel.connect(action["path"])
                data, ancillary, flags, _ = channel.recvmsg(1, socket.CMSG_SPACE(12))
                assert data == b"G" and not flags and len(ancillary) == 1
                level, kind, raw = ancillary[0]
                assert (level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS)
                descriptors = array.array("i", raw)
                assert len(descriptors) == 3
                sockets.append(socket.socket(fileno=descriptors[0]))
                leases.extend(descriptors[1:])
            emit({"taken": len(sockets) - 1})
        elif op == "admit":
            # Deliberately cache the userspace decision. The next actual send
            # must still fail if the driver kills/execs the owner in between.
            emit({"admitted": True})
        elif op in ("send", "helper"):
            connection = sockets[action["index"]]
            def attempt():
                try:
                    connection.sendall(b"synthetic-request\n")
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
            emit(result)
        elif op == "close":
            for endpoint, references in endpoints.values():
                endpoint.close()
                for fd in references:
                    os.close(fd)
            for connection in sockets + channels:
                connection.close()
            for fd in leases:
                os.close(fd)
            emit({"closed": True})
            return
        else:
            raise AssertionError(op)


def runtime():
    connections = []
    emit({"ready": True})
    for line in sys.stdin:
        action = json.loads(line)
        if action["op"] == "connect":
            connection = socket.create_connection(("127.0.0.1", action["port"]), timeout=3)
            connections.append(connection)
            emit({"connected": True})
        elif action["op"] == "send":
            try:
                connections[action.get("index", -1)].sendall(b"runtime-request\n")
                emit({"sent": True})
            except OSError as error:
                emit({"sent": False, "errno": error.errno})
        elif action["op"] == "exec":
            os.execl("/bin/sleep", "sleep", "60")
        else:
            raise AssertionError(action)


def adapter():
    """Stand-in adapter: relays actions to itself or to its two children."""
    children = {}
    emit({"ready": True})
    for line in sys.stdin:
        action = json.loads(line)
        if action["op"] == "spawn":
            for name in ("runtime", "sibling"):
                child = subprocess.Popen([sys.executable, __file__, "--runtime"],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
                assert json.loads(child.stdout.readline())["ready"]
                children[name] = child
            emit({name: child.pid for name, child in children.items()})
            continue
        target = action.pop("to")
        if target == "adapter":
            if action["op"] == "connect":
                connection = socket.create_connection(("127.0.0.1", action["port"]), timeout=3)
                emit({"connected": True})
            else:
                try:
                    connection.sendall(b"adapter-request\n")
                    emit({"sent": True})
                except OSError as error:
                    emit({"sent": False, "errno": error.errno})
            continue
        child = children[target]
        child.stdin.write(json.dumps(action) + "\n")
        child.stdin.flush()
        if action["op"] != "exec":
            print(child.stdout.readline(), end="", flush=True)


def set_frozen(cgroup, frozen):
    (cgroup / "cgroup.freeze").write_text("1" if frozen else "0")
    deadline = time.monotonic() + 5
    while f"frozen {int(frozen)}" not in (cgroup / "cgroup.events").read_text().split("\n"):
        assert time.monotonic() < deadline, ("cgroup freeze state not confirmed", frozen)
        time.sleep(0.01)


def descendant_scenario(executable):
    """A running descendant is enrolled only while its cgroup is frozen."""
    children, leases, maps = [], [], []
    cgroup = Path(f"/sys/fs/cgroup/louiselm-loader-descendant-{os.getpid()}")
    with tempfile.TemporaryDirectory(prefix="louiselm-loader-", dir="/var/tmp") as directory:
        root = Path(directory)
        root.chmod(0o755)
        broker_directory = root / "broker"
        broker_directory.mkdir(mode=0o700)
        os.chown(broker_directory, BROKER_UID, BROKER_UID)
        def spawn(arguments, uid=0):
            process = subprocess.Popen(arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True, user=uid, group=uid, extra_groups=[])
            children.append(process)
            return process
        cgroup.mkdir()
        try:
            broker_path = str(broker_directory / "control.sock")
            peer = spawn([sys.executable, __file__, "--broker", broker_path], BROKER_UID)
            assert receive(peer)["ready"]
            agent = spawn([sys.executable, __file__, "--adapter"], UID)
            assert receive(agent)["ready"]
            (cgroup / "cgroup.procs").write_text(str(agent.pid))
            pids = request(agent, {"op": "spawn"})
            owner = spawn([executable, "--exact", WORKER, "--ignored", "--nocapture"])
            send(owner, {"broker": broker_path, "session": "session-descendant",
                "runtime": pids["runtime"], "uid": UID, "executable": sys.executable,
                "descendant": True, "deadline_ms": 300000})
            assert receive(owner, True)["unfrozen_refused"]
            set_frozen(cgroup, True)
            send(owner, {"op": "frozen"})
            send(peer, {"op": "enrollment", "owner": owner.pid, "session": "session-descendant"})
            ready = receive(owner, True)
            assert ready["ready"]
            maps.extend(ready["maps"])
            leases.append(os.open(f"/proc/{owner.pid}/ns/mnt", os.O_RDONLY))
            assert receive(peer)["enrolled"]
            set_frozen(cgroup, False)
            denied = {"sent": False, "errno": errno.EPERM}
            for target in ("runtime", "sibling", "adapter"):
                assert request(agent, {"to": target, "op": "connect", "port": ready["port"]})["connected"]
            assert request(agent, {"to": "runtime", "op": "send"}) == denied
            assert request(owner, {"op": "activate"}, True)["activated"]
            assert request(agent, {"to": "runtime", "op": "send"})["sent"]
            for target in ("sibling", "adapter"):
                assert request(agent, {"to": target, "op": "send"}) == denied, target
            send(agent, {"to": "runtime", "op": "exec"})
            assert request(owner, {"op": "lost"}, True)["lost"]
            owner.kill()
            owner.wait(5)
            assert request(peer, {"op": "close"})["closed"]
            peer.wait(5)
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    process.kill()
                process.wait(5)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()
            (cgroup / "cgroup.kill").write_text("1")
            deadline = time.monotonic() + 5
            while (cgroup / "cgroup.procs").read_text().strip():
                assert time.monotonic() < deadline, "descendant fixture survived"
                time.sleep(0.01)
            cgroup.rmdir()
            for fd in leases:
                os.close(fd)
        assert_maps_released(maps)
    print("production Sender guard: descendant passed", flush=True)


def assert_maps_released(ids):
    library = ctypes.CDLL("libbpf.so.1", use_errno=True)
    library.bpf_map_get_fd_by_id.argtypes = [ctypes.c_uint32]
    deadline = time.monotonic() + 5
    while ids:
        remaining = []
        for identity in ids:
            fd = library.bpf_map_get_fd_by_id(identity)
            if fd >= 0:
                os.close(fd)
                remaining.append(identity)
            else:
                assert ctypes.get_errno() == errno.ENOENT
        ids = remaining
        assert time.monotonic() < deadline, ("guard maps survived cleanup", ids)
        if ids:
            time.sleep(0.01)


def scenario(executable, variant):
    children, observers, leases, maps = [], [], [], []
    with tempfile.TemporaryDirectory(prefix="louiselm-loader-", dir="/var/tmp") as directory:
        root = Path(directory)
        root.chmod(0o755)
        broker_directory = root / "broker"
        broker_directory.mkdir(mode=0o700)
        os.chown(broker_directory, BROKER_UID, BROKER_UID)
        def spawn(arguments, uid=0):
            process = subprocess.Popen(arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True, user=uid, group=uid, extra_groups=[])
            children.append(process)
            return process
        try:
            broker_path = str(broker_directory / "control.sock")
            peer = spawn([sys.executable, __file__, "--broker", broker_path], BROKER_UID)
            assert receive(peer)["ready"]
            upstream = socket.socket()
            observers.append(upstream)
            upstream.bind(("127.0.0.1", 0))
            upstream.listen(8)
            upstream.settimeout(5)
            destination = "127.0.0.1:" + str(upstream.getsockname()[1])
            owners, runtimes, ports, cookies = [], [], [], []
            for index in range(2):
                agent = spawn([sys.executable, __file__, "--runtime"], UID + index)
                assert receive(agent)["ready"]
                os.kill(agent.pid, signal.SIGSTOP)
                assert os.WIFSTOPPED(os.waitpid(agent.pid, os.WUNTRACED)[1])
                owner = spawn([executable, "--exact", WORKER, "--ignored", "--nocapture"])
                send(owner, {"broker": broker_path, "session": f"session-{index}",
                    "runtime": agent.pid, "uid": UID + index, "executable": sys.executable,
                    "deadline_ms": 8000 if variant == "expiry" and index == 0 else 300000})
                send(peer, {"op": "enrollment", "owner": owner.pid, "session": f"session-{index}"})
                ready = receive(owner, True)
                assert ready["ready"]
                maps.extend(ready["maps"])
                leases.append(os.open(f"/proc/{owner.pid}/ns/mnt", os.O_RDONLY))
                assert receive(peer)["enrolled"]
                assert request(owner, {"op": "protected"}, True)["protected"]
                assert request(owner, {"op": "stale"}, True)["stale_refused"]
                os.kill(agent.pid, signal.SIGCONT)
                assert request(agent, {"op": "connect", "port": ready["port"]})["connected"]
                assert request(agent, {"op": "send"}) == {"sent": False, "errno": errno.EPERM}
                assert request(owner, {"op": "activate"}, True)["activated"]
                assert request(agent, {"op": "send"})["sent"]
                handoff = str(root / f"handoff-{index}")
                send(owner, {"op": "connect", "address": destination, "handoff": handoff})
                assert receive(owner, True)["handoff"]
                assert request(peer, {"op": "take", "path": handoff})["taken"] == index
                connected = receive(owner, True)
                assert connected["connected"]
                cookies.append(connected["cookie"])
                observer, _ = upstream.accept()
                observer.settimeout(3)
                observers.append(observer)
                assert request(peer, {"op": "send", "index": index})["sent"]
                assert observer.recv(4096) == b"synthetic-request\n"
                assert request(peer, {"op": "helper", "index": index}) == {"sent": False, "errno": errno.EPERM}
                owners.append(owner)
                runtimes.append(agent)
                ports.append(ready["port"])
            assert request(runtimes[0], {"op": "connect", "port": ports[1]})["connected"]
            assert request(runtimes[0], {"op": "send"}) == {"sent": False, "errno": errno.EPERM}
            assert request(peer, {"op": "admit"})["admitted"]
            if variant == "exec":
                send(owners[0], {"op": "exec"})
                deadline = time.monotonic() + 5
                while os.readlink(f"/proc/{owners[0].pid}/exe") != "/usr/bin/sleep":
                    assert time.monotonic() < deadline
                    time.sleep(0.01)
            elif variant == "runtime-exit":
                runtimes[0].kill()
                runtimes[0].wait(5)
                assert request(owners[0], {"op": "lost"}, True)["lost"]
            elif variant == "runtime-exec":
                send(runtimes[0], {"op": "exec"})
                assert request(owners[0], {"op": "lost"}, True)["lost"]
            elif variant == "revision":
                send(peer, {"op": "close_endpoint", "channel": 0})
                send(owners[0], {"op": "revise"})
                assert receive(peer)["endpoint_closed"]
                send(peer, {"op": "enrollment", "new": False, "channel": 0,
                    "owner": owners[0].pid, "session": "session-0", "revision": 2})
                assert receive(owners[0], True)["revised"]
                assert receive(peer)["enrolled"]
                assert request(owners[0], {"op": "activate"}, True)["activated"]
            elif variant == "broker-crash":
                peer.kill()
                peer.wait(5)
                for owner in owners:
                    assert request(owner, {"op": "lost"}, True)["lost"]
            elif variant == "retire":
                assert request(owners[0], {"op": "retire", "cookie": cookies[0]}, True)["retired"]
            elif variant == "expiry":
                assert request(runtimes[0], {"op": "connect", "port": ports[0]})["connected"]
                assert request(runtimes[0], {"op": "send"})["sent"]
                time.sleep(8.5)
                assert request(runtimes[0], {"op": "send"}) == {"sent": False, "errno": errno.EPERM}
            elif variant == "dispose":
                send(peer, {"op": "close_endpoint", "channel": 0})
                send(owners[0], {"op": "close"})
                assert receive(peer)["endpoint_closed"]
                assert owners[0].wait(5) == 0
            else:
                owners[0].kill()
                owners[0].wait(5)
            if variant != "broker-crash":
                denied = request(peer, {"op": "send", "index": 0})
                assert denied["sent"] is False, ("owner loss crossed actual upstream write", variant, denied)
                assert denied["errno"] == (errno.EPIPE if variant == "retire" else errno.EPERM), denied
                assert request(peer, {"op": "send", "index": 1})["sent"]
                assert observers[2].recv(4096) == b"synthetic-request\n"
            # Ordinary traffic to precisely the same upstream remains usable.
            with socket.create_connection(upstream.getsockname(), timeout=3) as ordinary:
                observer, _ = upstream.accept()
                observers.append(observer)
                ordinary.sendall(b"unrelated\n")
                assert observer.recv(4096) == b"unrelated\n"
            if variant != "broker-crash":
                assert request(peer, {"op": "close"})["closed"]
                peer.wait(5)
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    process.kill()
                process.wait(5)
                for stream in (process.stdin, process.stdout, process.stderr):
                    stream.close()
            for connection in observers:
                connection.close()
            for fd in leases:
                os.close(fd)
        assert_maps_released(maps)
    print(f"production Sender guard: {variant} passed", flush=True)


def main():
    if sys.argv[1:2] == ["--broker"]:
        broker(sys.argv[2])
        return
    if sys.argv[1:2] == ["--runtime"]:
        runtime()
        return
    if sys.argv[1:2] == ["--adapter"]:
        adapter()
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library_tests", type=Path, help="Exact library-test executable, or its clean target/debug/deps directory")
    args = parser.parse_args()
    assert os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    assert "bpf" in Path("/sys/kernel/security/lsm").read_text().split(",")
    executable = args.library_tests.resolve()
    if executable.is_dir():
        candidates = [path for path in executable.glob("louiselm_skills-*") if path.is_file() and os.access(path, os.X_OK)]
        assert len(candidates) == 1, "expected one library-test executable in clean target directory"
        executable = candidates[0]
    for variant in ("owner-death", "exec", "runtime-exit", "runtime-exec", "revision", "broker-crash", "retire", "expiry", "dispose"):
        scenario(str(executable), variant)
    descendant_scenario(str(executable))
    print("PRODUCTION_SENDER_GUARD_COMPONENT_PASS_NOT_VERIFIED", flush=True)


if __name__ == "__main__":
    main()
