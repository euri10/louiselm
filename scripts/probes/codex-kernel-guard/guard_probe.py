"""Disposable VM socket-authority experiment; synthetic traffic only."""
import ctypes as C
import errno
import http.client
import http.server
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time


class Rule(C.Structure):
    _fields_ = [("tgid", C.c_uint32), ("port", C.c_uint32),
                ("deadline", C.c_uint64), ("checks", C.c_uint64)]


class Guard:
    def __init__(self, path):
        self.lib = C.CDLL("libbpf.so.1", use_errno=True)
        signatures = {
            "bpf_object__open_file": (C.c_void_p, [C.c_char_p, C.c_void_p]),
            "libbpf_get_error": (C.c_long, [C.c_void_p]),
            "bpf_object__load": (C.c_int, [C.c_void_p]),
            "bpf_object__find_program_by_name": (C.c_void_p, [C.c_void_p, C.c_char_p]),
            "bpf_program__attach_lsm": (C.c_void_p, [C.c_void_p]),
            "bpf_object__find_map_fd_by_name": (C.c_int, [C.c_void_p, C.c_char_p]),
            "bpf_map_update_elem": (C.c_int, [C.c_int, C.c_void_p, C.c_void_p, C.c_uint64]),
            "bpf_map_lookup_elem": (C.c_int, [C.c_int, C.c_void_p, C.c_void_p]),
            "bpf_link__destroy": (C.c_int, [C.c_void_p]),
            "bpf_object__close": (None, [C.c_void_p]),
        }
        for name, (result, args) in signatures.items():
            function = getattr(self.lib, name)
            function.restype, function.argtypes = result, args
        self.obj = self.lib.bpf_object__open_file(os.fsencode(path), None)
        assert self.obj and self.lib.libbpf_get_error(self.obj) == 0
        assert self.lib.bpf_object__load(self.obj) == 0, "BPF verifier rejected probe"
        program = self.lib.bpf_object__find_program_by_name(self.obj, b"endpoint_send")
        assert program
        self.link = self.lib.bpf_program__attach_lsm(program)
        assert self.link and self.lib.libbpf_get_error(self.link) == 0
        self.fd = self.lib.bpf_object__find_map_fd_by_name(self.obj, b"policy")
        assert self.fd >= 0

    def set(self, pid, port, deadline=None):
        key = C.c_uint32(0)
        rule = Rule(pid, port, time.monotonic_ns() + 60_000_000_000 if deadline is None else deadline, 0)
        assert self.lib.bpf_map_update_elem(self.fd, C.byref(key), C.byref(rule), 0) == 0

    def checks(self):
        key, rule = C.c_uint32(0), Rule()
        assert self.lib.bpf_map_lookup_elem(self.fd, C.byref(key), C.byref(rule)) == 0
        return rule.checks

    def close(self):
        assert self.lib.bpf_link__destroy(self.link) == 0
        self.lib.bpf_object__close(self.obj)


def transmit(connection, method):
    payload = b"POST /fixture HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
    try:
        if method == "send":
            connection.sendall(payload)
        elif method == "sendmsg":
            assert connection.sendmsg([payload]) == len(payload)
        elif method == "write":
            assert os.write(connection.fileno(), payload) == len(payload)
        elif method == "writev":
            assert os.writev(connection.fileno(), [payload]) == len(payload)
        elif method == "sendfile":
            with tempfile.TemporaryFile() as source:
                source.write(payload)
                source.flush()
                assert os.sendfile(connection.fileno(), source.fileno(), 0, len(payload)) == len(payload)
        elif method == "splice":
            read_fd, write_fd = os.pipe()
            try:
                os.write(write_fd, payload)
                assert os.splice(read_fd, connection.fileno(), len(payload)) == len(payload)
            finally:
                os.close(read_fd)
                os.close(write_fd)
        elif method == "sendmmsg":
            class Iovec(C.Structure):
                _fields_ = [("base", C.c_void_p), ("length", C.c_size_t)]
            class Msghdr(C.Structure):
                _fields_ = [("name", C.c_void_p), ("namelen", C.c_uint32),
                            ("iov", C.POINTER(Iovec)), ("iovlen", C.c_size_t),
                            ("control", C.c_void_p), ("controllen", C.c_size_t),
                            ("flags", C.c_int)]
            class Mmsghdr(C.Structure):
                _fields_ = [("header", Msghdr), ("length", C.c_uint32)]
            address = C.create_string_buffer(struct.pack("=H", socket.AF_INET)
                + struct.pack("!H", connection.getpeername()[1])
                + socket.inet_aton("127.0.0.1") + b"\0" * 8)
            data = C.create_string_buffer(payload)
            iov = Iovec(C.cast(data, C.c_void_p), len(payload))
            messages = (Mmsghdr * 2)()
            for message in messages:
                message.header = Msghdr(C.cast(address, C.c_void_p), 16, C.pointer(iov), 1, None, 0, 0)
            libc = C.CDLL(None, use_errno=True)
            libc.sendmmsg.argtypes = [C.c_int, C.POINTER(Mmsghdr), C.c_uint, C.c_int]
            count = libc.sendmmsg(connection.fileno(), messages, 2, 0)
            if count < 0:
                raise OSError(C.get_errno(), "sendmmsg")
            assert count == 2, count
            raw = b""
            while raw.count(b"\r\n\r\n") < 2:
                chunk = connection.recv(4096)
                assert chunk
                raw += chunk
            assert raw.count(b"HTTP/1.1 200") == 2
            return {"accepted": True, "requests": 2}
        else:
            raise ValueError(method)
        response = http.client.HTTPResponse(connection)
        response.begin()
        assert response.status == 200
        response.read()
        return {"accepted": True}
    except OSError as error:
        if error.errno != errno.EPERM:
            raise
        return {"accepted": False, "errno": error.errno}


def child(port):
    connection = None
    print(json.dumps({"ready": os.getpid()}), flush=True)
    try:
        for line in sys.stdin:
            action = json.loads(line)
            if action == "open":
                connection = socket.create_connection(("127.0.0.1", port), timeout=3)
                result = {"connected": True}
            elif action.startswith("fork:"):
                read_fd, write_fd = os.pipe()
                pid = os.fork()
                if pid == 0:
                    os.close(read_fd)
                    try:
                        result = transmit(connection, action.split(":")[1])
                        os.write(write_fd, json.dumps(result).encode())
                        os._exit(0)
                    except BaseException:
                        os._exit(1)
                os.close(write_fd)
                raw = os.read(read_fd, 4096)
                os.close(read_fd)
                assert os.waitpid(pid, 0)[1] == 0
                result = json.loads(raw)
            else:
                result = transmit(connection, action)
            print(json.dumps(result), flush=True)
    finally:
        if connection is not None:
            connection.close()


def receive(process):
    assert select.select([process.stdout], [], [], 5)[0], "child response timeout"
    line = process.stdout.readline()
    assert line, "child exited"
    return json.loads(line)


def request(process, action):
    process.stdin.write(json.dumps(action) + "\n")
    process.stdin.flush()
    return receive(process)


def main():
    assert sys.argv[1] == "--disposable-vm"
    assert os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    assert "bpf" in Path("/sys/kernel/security/lsm").read_text().split(",")
    seen = []

    class Endpoint(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        def log_message(self, *_args):
            pass
        def do_POST(self):
            seen.append(self.path)
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    guard = Guard(sys.argv[2])
    process = subprocess.Popen([sys.executable, __file__, "--child", str(server.server_port)],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
                               user=65534, group=65534, extra_groups=[])
    report = {}
    try:
        assert receive(process)["ready"] == process.pid
        pin = os.pidfd_open(process.pid)
        try:
            guard.set(process.pid, server.server_port)
            request(process, "open")
            for method in ("send", "sendmsg", "write", "writev", "sendfile", "splice", "sendmmsg"):
                before = len(seen)
                checks_before = guard.checks()
                allowed = request(process, method)
                checks_after = guard.checks()
                denied = request(process, "fork:" + method)
                assert allowed["accepted"] and not denied["accepted"], (method, allowed, denied)
                assert len(seen) == before + (2 if method == "sendmmsg" else 1)
                report[method] = {"authorized": allowed, "inherited_helper": denied,
                                  "authorized_send_hook_checks": checks_after - checks_before}
            guard.set(0, server.server_port)
            assert not request(process, "send")["accepted"]
            report["revoked_existing_socket"] = "denied"
            guard.set(process.pid, server.server_port, time.monotonic_ns() - 1)
            assert not request(process, "send")["accepted"]
            report["expired_existing_socket"] = "denied"
            guard.set(process.pid, server.server_port)
            assert request(process, "send")["accepted"]
            report["fresh_authorization"] = "accepted"
            guard.close()
            guard = None
            assert request(process, "fork:send")["accepted"]
            report["detached_guard_helper"] = "accepted: fail-open without guard lifetime design"
        finally:
            os.close(pin)
        print(json.dumps({"kernel": os.uname().release, "results": report,
                          "verdict": "PRIMITIVE_ONLY_NOT_VERIFIED"}, indent=2))
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        if guard is not None:
            guard.close()
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--child":
        child(int(sys.argv[2]))
    else:
        main()
