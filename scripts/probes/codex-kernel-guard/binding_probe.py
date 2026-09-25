"""Opt-in exact sender/endpoint binding proof; root in a disposable KVM only."""
import array
import ctypes as C
import errno
import http.client
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import threading
import time

from guard_probe import Guard, receive, request, transmit

SESSION_UID = 4020000


class Grant(C.Structure):
    _fields_ = [(name, C.c_uint64) for name in ("launch", "session", "run", "revoked")]


class Binding(C.Structure):
    _fields_ = [(name, C.c_uint64) for name in
               ("launch", "session", "run", "revision", "listener", "deadline")] + [
                   ("netns", C.c_uint32), ("family", C.c_uint32),
                   ("address", C.c_uint32 * 4)]


class BindingGuard(Guard):
    def __init__(self, path):
        super().__init__(path)
        self.extra_links = []
        self.pins = []
        self.namespace = os.open("/proc/self/ns/net", os.O_RDONLY)
        self.lib.bpf_program__attach_trace.restype = C.c_void_p
        self.lib.bpf_program__attach_trace.argtypes = [C.c_void_p]
        self.lib.bpf_map_delete_elem.restype = C.c_int
        self.lib.bpf_map_delete_elem.argtypes = [C.c_int, C.c_void_p]
        try:
            for name, attach in ((b"invalidate_exec", self.lib.bpf_program__attach_lsm),
                                 (b"protect_runtime", self.lib.bpf_program__attach_lsm),
                                 (b"invalidate_listener", self.lib.bpf_program__attach_trace)):
                program = self.lib.bpf_object__find_program_by_name(self.obj, name)
                assert program, name
                link = attach(program)
                assert link and self.lib.libbpf_get_error(link) == 0, name
                self.extra_links.append(link)
        except BaseException:
            self.close()
            raise

    def map_fd(self, name):
        fd = self.lib.bpf_object__find_map_fd_by_name(self.obj, name.encode())
        assert fd >= 0, name
        return fd

    def put(self, name, key, value, flags=0):
        assert self.lib.bpf_map_update_elem(self.map_fd(name), C.byref(key),
                                            C.byref(value), flags) == 0, (name, C.get_errno())

    def enroll(self, process, launch, session, run, executable):
        # Production uses launch_transport::KernelProcess at the supervisor's
        # measured exec stop. Here all threads stop before inspecting/enrolling;
        # no kernel grant exists during the preceding fixture startup.
        pin = os.pidfd_open(process.pid)
        self.pins.append(pin)
        signal.pidfd_send_signal(pin, signal.SIGSTOP)
        pid, status = os.waitpid(process.pid, os.WUNTRACED)
        assert pid == process.pid and os.WIFSTOPPED(status)
        try:
            assert os.path.samefile(f"/proc/{pid}/exe", executable)
            self.put("tasks", C.c_int(pin), Grant(launch, session, run, 0), 1)
        finally:
            signal.pidfd_send_signal(pin, signal.SIGCONT)
        return pin

    def reserve(self, server):
        key, reserved = C.c_uint32(server.server_port), C.c_uint32()
        if self.lib.bpf_map_lookup_elem(self.map_fd("ports"), C.byref(key), C.byref(reserved)) == 0:
            assert reserved.value == os.fstat(self.namespace).st_ino
            return
        assert C.get_errno() == errno.ENOENT
        self.put("ports", key, C.c_uint32(os.fstat(self.namespace).st_ino))

    def publish(self, server, launch=1, session=101, run=201, revision=1, deadline=None):
        self.reserve(server)
        listener = struct.unpack("Q", server.socket.getsockopt(socket.SOL_SOCKET, 57, 8))[0]
        self.put("listeners", C.c_uint64(listener), C.c_uint32(1))
        rule = Binding(launch, session, run, revision, listener,
                       time.monotonic_ns() + 120_000_000_000 if deadline is None else deadline,
                       os.fstat(self.namespace).st_ino, server.address_family)
        address = socket.inet_pton(server.address_family, server.server_address[0])
        for index in range(len(address) // 4):
            rule.address[index] = struct.unpack_from("=I", address, index * 4)[0]
        self.put("policy", C.c_uint32(server.server_port), rule)
        return rule

    def close(self):
        for link in reversed(self.extra_links):
            assert self.lib.bpf_link__destroy(link) == 0
        for pin in self.pins:
            os.close(pin)
        os.close(self.namespace)
        super().close()


def child():
    connections = []
    print(json.dumps({"ready": os.getpid()}), flush=True)
    for line in sys.stdin:
        action = json.loads(line)
        operation = action["op"]
        if operation == "open":
            connection = socket.create_connection((action["address"], action["port"]), timeout=3)
            connections.append(connection)
            result = {"connected": len(connections) - 1}
        elif operation == "send":
            result = transmit(connections[action.get("index", -1)], action.get("method", "send"))
        elif operation == "queue":
            connections[-1].sendall(b"POST /fixture HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            result = {"queued": True}
        elif operation == "reply":
            response = http.client.HTTPResponse(connections[-1])
            response.begin()
            response.read()
            result = {"status": response.status}
        elif operation == "eof":
            result = {"closed": connections[-1].recv(1) == b""}
        elif operation == "thread":
            result = []
            worker = threading.Thread(target=lambda: result.append(transmit(connections[-1], "send")))
            worker.start()
            worker.join(5)
            assert not worker.is_alive() and len(result) == 1
            result = result[0]
        elif operation == "fork":
            read_fd, write_fd = os.pipe()
            pid = os.fork()
            if pid == 0:
                os.close(read_fd)
                try:
                    value = transmit(connections[-1], action.get("method", "send"))
                    os.write(write_fd, json.dumps(value).encode())
                    os._exit(0)
                except BaseException:
                    os._exit(1)
            os.close(write_fd)
            raw = os.read(read_fd, 4096)
            os.close(read_fd)
            assert os.waitpid(pid, 0)[1] == 0
            result = json.loads(raw)
        elif operation == "pass":
            # SCM_RIGHTS reaches an independently exec'd sibling, not a fork
            # retaining authority or the controller's privileged map fds.
            with socket.socket(fileno=action["fd"]) as channel:
                channel.sendmsg([b"S"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS,
                                         array.array("i", [connections[-1].fileno()]))])
            result = {"passed": True}
        elif operation == "take":
            with socket.socket(fileno=action["fd"]) as channel:
                data, ancillary, flags, _ = channel.recvmsg(1, socket.CMSG_SPACE(4))
                assert data == b"S" and not flags and len(ancillary) == 1
                level, kind, raw = ancillary[0]
                assert (level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS)
                descriptors = array.array("i")
                descriptors.frombytes(raw)
                assert len(descriptors) == 1
                connection = socket.socket(fileno=descriptors[0])
                connection.settimeout(3)
                connections.append(connection)
            result = transmit(connection, "send")
        elif operation in ("exec", "thread-exec"):
            os.set_inheritable(connections[-1].fileno(), True)
            args = [sys.executable, __file__, "--after-exec", str(connections[-1].fileno())]
            if operation == "exec":
                os.execv(sys.executable, args)
            worker = threading.Thread(target=lambda: os.execv(sys.executable, args))
            worker.start()
            worker.join(5)
            raise AssertionError("thread exec did not replace process")
        elif operation == "setns":
            os.setns(action["fd"], os.CLONE_NEWNET)
            os.setgroups([])
            os.setgid(SESSION_UID)
            os.setuid(SESSION_UID)
            result = transmit(connections[-1], "send")
        elif operation == "enter-netns":
            os.setns(action["fd"], os.CLONE_NEWNET)
            os.setgroups([])
            os.setgid(SESSION_UID)
            os.setuid(SESSION_UID)
            result = {"entered": True}
        elif operation == "map-access":
            # BPF_MAP_GET_FD_BY_ID, without receiving a writable map descriptor.
            attr = (C.c_uint32 * 8)(action["id"])
            libc = C.CDLL(None, use_errno=True)
            result_fd = libc.syscall(321, 14, C.byref(attr), C.sizeof(attr))
            result = {"fd": result_fd, "errno": C.get_errno()}
            assert result == {"fd": -1, "errno": errno.EPERM}, result
            try:
                os.open(action["proc_fd"], os.O_RDWR)
            except PermissionError:
                result["proc_denied"] = True
            else:
                raise AssertionError("tool opened controller map descriptor")
        else:
            raise ValueError(operation)
        print(json.dumps(result), flush=True)


class Endpoint:
    def __init__(self, family=socket.AF_INET, port=0, address=None, session=101):
        self.seen = []
        seen = self.seen

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            def log_message(self, *_args):
                pass
            def do_POST(self):
                # Broker association comes from its dedicated listener owner,
                # never a peer-selected header, PID, source port or bearer.
                seen.append({"session": session, "path": self.path})
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

        class Server(http.server.ThreadingHTTPServer):
            address_family = family

        self.server = Server((address or ("127.0.0.1" if family == socket.AF_INET else "::1"), port), Handler)
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.worker.start()

    def close_listener(self):
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(5)
        assert not self.worker.is_alive()


def spawn(extra_fds=(), uid=SESSION_UID):
    process = subprocess.Popen([sys.executable, __file__, "--child"],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
                               user=uid, group=uid, extra_groups=[], pass_fds=extra_fds)
    assert receive(process)["ready"] == process.pid
    return process


def open_connection(process, endpoint):
    server = endpoint.server
    return request(process, {"op": "open", "address": server.server_address[0],
                             "port": server.server_port})


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(5)
    process.stdin.close()
    process.stdout.close()


def main():
    assert sys.argv[1] == "--disposable-vm" and os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    endpoints, children = [], []
    guard = BindingGuard(sys.argv[2])
    report = {}
    try:
        endpoint = Endpoint()
        endpoints.append(endpoint)
        guard.reserve(endpoint.server)
        sender = spawn()
        children.append(sender)
        open_connection(sender, endpoint)
        assert not request(sender, {"op": "send"})["accepted"]
        report["unpublished_binding"] = "denied"
        guard.enroll(sender, 1, 101, 201, sys.executable)
        rule = guard.publish(endpoint.server)
        for method in ("send", "sendmsg", "write", "writev", "sendfile", "splice", "sendmmsg"):
            before = len(endpoint.seen)
            assert request(sender, {"op": "send", "method": method})["accepted"]
            assert not request(sender, {"op": "fork", "method": method})["accepted"]
            assert len(endpoint.seen) == before + (2 if method == "sendmmsg" else 1)
        report["all_seven_send_paths"] = "authorized sender accepted; inherited helper EPERM"
        assert request(sender, {"op": "thread"})["accepted"]
        report["runtime_thread"] = "accepted"

        sibling = spawn(uid=SESSION_UID + 1)
        children.append(sibling)
        guard.enroll(sibling, 2, 102, 201, sys.executable)
        open_connection(sibling, endpoint)
        assert not request(sibling, {"op": "send"})["accepted"]
        sibling_endpoint = Endpoint(session=102)
        endpoints.append(sibling_endpoint)
        guard.publish(sibling_endpoint.server, launch=2, session=102)
        open_connection(sibling, sibling_endpoint)
        assert request(sibling, {"op": "send"})["accepted"]
        open_connection(sender, sibling_endpoint)
        assert not request(sender, {"op": "send"})["accepted"]
        assert sibling_endpoint.seen == [{"session": 102, "path": "/fixture"}]
        assert all(row["session"] == 101 for row in endpoint.seen)
        open_connection(sender, endpoint)
        assert request(sender, {"op": "send"})["accepted"]
        report["other_session"] = "denied"
        report["broker_association"] = "two dedicated listeners retain their own Session; cross-sends denied"
        for field in ("launch", "session", "run"):
            old = getattr(rule, field)
            setattr(rule, field, old + 1)
            guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)
            assert not request(sender, {"op": "send"})["accepted"]
            setattr(rule, field, old)
        guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)
        assert request(sender, {"op": "send"})["accepted"]
        report["binding_dimensions"] = "launch, Session and Run mismatches denied"

        # Map updates copy an immutable HASH record atomically. Alternate two
        # wholly invalid records whose torn combination would authorize sender.
        first = Binding.from_buffer_copy(rule)
        first.session += 1
        second = Binding.from_buffer_copy(rule)
        second.run += 1
        guard.put("policy", C.c_uint32(endpoint.server.server_port), first)
        done, failures = threading.Event(), []
        def update():
            try:
                while not done.is_set():
                    guard.put("policy", C.c_uint32(endpoint.server.server_port), first)
                    guard.put("policy", C.c_uint32(endpoint.server.server_port), second)
            except BaseException as error:
                failures.append(str(error))
        updater = threading.Thread(target=update)
        updater.start()
        try:
            before = len(endpoint.seen)
            for _ in range(200):
                assert not request(sender, {"op": "send"})["accepted"]
            assert len(endpoint.seen) == before
        finally:
            done.set()
            updater.join(5)
        assert not updater.is_alive() and not failures, failures
        guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)
        assert request(sender, {"op": "send"})["accepted"]
        report["atomic_publication"] = "200 concurrent sends denied; no mixed-record authority"

        rule.revision += 1
        guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)
        assert not request(sender, {"op": "send"})["accepted"]
        open_connection(sender, endpoint)
        assert request(sender, {"op": "send"})["accepted"]
        report["revision"] = "old connection denied; fresh connection accepted"
        rule.deadline = time.monotonic_ns() - 1
        guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)
        assert not request(sender, {"op": "send"})["accepted"]
        rule.deadline = time.monotonic_ns() + 120_000_000_000
        guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)
        assert request(sender, {"op": "send"})["accepted"]
        report["expiry"] = "denied"

        # A distinct exec'd process receives an authorized connection via SCM_RIGHTS.
        left, right = socket.socketpair()
        try:
            giver = spawn((left.fileno(),))
            taker = spawn((right.fileno(),))
            children.extend([giver, taker])
            guard.enroll(giver, 3, 103, 201, sys.executable)
            guard.publish(endpoint.server, launch=3, session=103, revision=2)
            open_connection(giver, endpoint)
            assert request(giver, {"op": "send"})["accepted"]
            assert request(giver, {"op": "pass", "fd": left.fileno()})["passed"]
            assert not request(taker, {"op": "take", "fd": right.fileno()})["accepted"]
            report["scm_rights"] = "independent recipient denied"
        finally:
            left.close()
            right.close()
        guard.put("policy", C.c_uint32(endpoint.server.server_port), rule)

        # Freeze-time identity plus task storage survives PID equality but not exec.
        assert not request(sender, {"op": "exec"})["accepted"]
        assert not request(sender, {"op": "send"})["accepted"]
        report["same_pid_same_executable_exec"] = "permanently denied"
        old_pid = sender.pid
        stop(sender)
        children.remove(sender)
        replacement = None
        for _ in range(20):
            Path("/proc/sys/kernel/ns_last_pid").write_text(str(old_pid - 1))
            candidate = spawn()
            if candidate.pid == old_pid:
                replacement = candidate
                children.append(candidate)
                break
            stop(candidate)
        assert replacement is not None, "actual numeric PID reuse not established"
        open_connection(replacement, endpoint)
        assert not request(replacement, {"op": "send"})["accepted"]
        report["actual_pid_reuse"] = {"pid": old_pid, "accepted": False, "old_pidfd_retained": True}

        # An endpoint is the broker's retained listener, not its reusable tuple.
        old_port = endpoint.server.server_port
        guard.publish(endpoint.server, launch=3, session=103, revision=2)
        assert request(giver, {"op": "send"})["accepted"]
        endpoint.close_listener()
        assert not request(giver, {"op": "send"})["accepted"]
        replacement_endpoint = Endpoint(port=old_port)
        endpoints.append(replacement_endpoint)
        open_connection(giver, replacement_endpoint)
        assert not request(giver, {"op": "send"})["accepted"]
        assert not replacement_endpoint.seen
        guard.publish(replacement_endpoint.server, launch=3, session=103, revision=3)
        assert request(giver, {"op": "send"})["accepted"]
        report["listener_reuse"] = "denied until explicit fresh listener publication"

        ipv6 = Endpoint(socket.AF_INET6)
        endpoints.append(ipv6)
        guard.publish(ipv6.server, launch=3, session=103)
        open_connection(giver, ipv6)
        assert request(giver, {"op": "send"})["accepted"]
        assert not request(giver, {"op": "fork"})["accepted"]
        report["ipv6"] = "authorized accepted; helper denied"
        alias = Endpoint(port=replacement_endpoint.server.server_port, address="127.0.0.2")
        endpoints.append(alias)
        open_connection(giver, alias)
        assert not request(giver, {"op": "send"})["accepted"]
        assert not alias.seen
        report["address_alias"] = "denied"

        # Same tuple in another namespace is unrelated traffic (louiselm-8a8id).
        # It gains no authority to our endpoint. Keep
        # namespace fds open so an inode cannot be recycled during the binding.
        keeper = subprocess.Popen([sys.executable, __file__, "--netns-endpoint", str(old_port)],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        children.append(keeper)
        assert receive(keeper)["port"] == old_port
        namespace = os.open(f"/proc/{keeper.pid}/ns/net", os.O_RDONLY)
        try:
            foreign = spawn((namespace,), uid=0)
            children.append(foreign)
            assert request(foreign, {"op": "enter-netns", "fd": namespace})["entered"]
            guard.enroll(foreign, 4, 104, 201, sys.executable)
            guard.publish(replacement_endpoint.server, launch=4, session=104, revision=4)
            open_connection(foreign, replacement_endpoint)
            assert request(foreign, {"op": "send"})["accepted"]
            report["other_socket_namespace"] = "unrelated namespace unaffected at the same address/port"

            mover = spawn((namespace,), uid=0)
            children.append(mover)
            guard.enroll(mover, 5, 105, 201, sys.executable)
            guard.publish(replacement_endpoint.server, launch=5, session=105, revision=5)
            open_connection(mover, replacement_endpoint)
            assert request(mover, {"op": "send"})["accepted"]
            assert not request(mover, {"op": "setns", "fd": namespace})["accepted"]
            report["other_sender_namespace"] = "original connected socket denied after namespace switch and UID drop"
        finally:
            os.close(namespace)

        map_fd = guard.map_fd("policy")
        info = Path(f"/proc/self/fdinfo/{map_fd}").read_text()
        map_id = int(next(line.split()[1] for line in info.splitlines() if line.startswith("map_id:")))
        assert request(sibling, {"op": "map-access", "id": map_id,
                                 "proc_fd": f"/proc/{os.getpid()}/fd/{map_fd}"})["proc_denied"]
        report["map_custody"] = "BPF map-id lookup and controller fd access denied"
        threaded = spawn()
        children.append(threaded)
        guard.enroll(threaded, 6, 106, 201, sys.executable)
        guard.publish(replacement_endpoint.server, launch=6, session=106, revision=6)
        open_connection(threaded, replacement_endpoint)
        assert request(threaded, {"op": "thread"})["accepted"]
        assert not request(threaded, {"op": "thread-exec"})["accepted"]
        report["nonleader_exec"] = "runtime thread accepted before exec; replacement denied"
        print(json.dumps({"verdict": "BINDING_COMPONENT_PASS_NOT_VERIFIED",
                          "kernel": os.uname().release, "results": report}, indent=2))
    finally:
        for process in children:
            stop(process)
        for endpoint in endpoints:
            endpoint.close_listener()
        guard.close()


class StockGuard(BindingGuard):
    def __init__(self, path, server):
        super().__init__(path)
        self.server = server
        self.enrolled = False

    def set(self, pid, port):
        assert port == self.server.server_port
        self.reserve(self.server)
        if pid:
            assert not self.enrolled
            # Stock fixture keeps process ownership; enrollment only needs pid.
            class Process:
                pass
            process = Process()
            process.pid = pid
            self.enroll(process, 1, 101, 201, "/var/tmp/ow3ok-codex")
            self.enrolled = True
            self.publish(self.server)
        elif self.enrolled:
            key = C.c_uint32(port)
            assert self.lib.bpf_map_delete_elem(self.map_fd("policy"), C.byref(key)) == 0


if __name__ == "__main__":
    if sys.argv[1] == "--child":
        child()
    elif sys.argv[1] == "--after-exec":
        with socket.socket(fileno=int(sys.argv[2])) as connection:
            connection.settimeout(3)
            print(json.dumps(transmit(connection, "send")), flush=True)
            for line in sys.stdin:
                assert json.loads(line)["op"] == "send"
                print(json.dumps(transmit(connection, "send")), flush=True)
    elif sys.argv[1] == "--netns-endpoint":
        os.unshare(os.CLONE_NEWNET)
        subprocess.run(["ip", "link", "set", "lo", "up"], check=True, timeout=5)
        endpoint = Endpoint(port=int(sys.argv[2]))
        print(json.dumps({"port": endpoint.server.server_port}), flush=True)
        try:
            sys.stdin.read()
        finally:
            endpoint.close_listener()
    elif "--stock" in sys.argv:
        import stock_probe
        stock_probe.main(StockGuard, "STOCK_CODEX_BINDING_PASS_NOT_VERIFIED")
    else:
        main()
