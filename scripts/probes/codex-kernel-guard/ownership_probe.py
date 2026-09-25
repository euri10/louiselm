"""Disposable proof of endpoint-owned, read-only guard pins; no installed claim."""
import ctypes as C
from concurrent.futures import ThreadPoolExecutor
import errno
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import threading
import time
from types import SimpleNamespace

from binding_probe import Binding, BindingGuard, Endpoint, Grant, spawn, stop
from guard_probe import receive, request, transmit
from lifetime_probe import api, require_guest


class LifecycleGuard(BindingGuard):
    def __init__(self, path):
        super().__init__(path)
        self.lib.bpf_program__attach_raw_tracepoint.restype = C.c_void_p
        self.lib.bpf_program__attach_raw_tracepoint.argtypes = [C.c_void_p, C.c_char_p]
        program = self.lib.bpf_object__find_program_by_name(self.obj, b"owner_exit")
        assert program
        link = self.lib.bpf_program__attach_raw_tracepoint(program, b"sched_process_exit")
        assert link and self.lib.libbpf_get_error(link) == 0
        self.extra_links.append(link)
        program = self.lib.bpf_object__find_program_by_name(self.obj, b"owner_exec")
        assert program
        link = self.lib.bpf_program__attach_lsm(program)
        assert link and self.lib.libbpf_get_error(link) == 0
        self.extra_links.append(link)


class OwnedEndpoint:
    """Single admission lock; no cached permit crosses a control transition."""
    def __init__(self, guard, upstream_port, port=0):
        self.lib = guard.lib
        self.maps = {name: os.dup(guard.map_fd(name)) for name in ("lost", "policy")}
        self.lock = threading.Lock()
        self.release = threading.Event()
        self.release.set()
        self.waiting = threading.Event()
        self.effect_release = threading.Event()
        self.effect_release.set()
        self.effect_waiting = threading.Event()
        self.upstream = socket.create_connection(("127.0.0.1", upstream_port), timeout=3)
        self.connections = set()
        self.seen = []
        self.active = False
        self.binding = None
        self.closed = False
        endpoint = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def setup(self):
                self.request.settimeout(5)
                super().setup()

            def finish(self):
                try:
                    super().finish()
                finally:
                    with endpoint.lock:
                        endpoint.connections.discard(self.connection)

            def do_POST(self):
                endpoint.waiting.set()
                assert endpoint.release.wait(5), "admission barrier timed out"
                with endpoint.lock:
                    allowed = endpoint.active and endpoint.authorized()
                    if allowed:
                        endpoint.effect_waiting.set()
                        assert endpoint.effect_release.wait(5), "effect barrier timed out"
                        # Reuse the same actual-sender guard at the final write.
                        # A supervisor exit after authorized() cannot turn its
                        # cached result into an unguarded synthetic upstream send.
                        allowed = transmit(endpoint.upstream, "send")["accepted"]
                        if allowed:
                            endpoint.seen.append(self.path)
                    self.send_response(200 if allowed else 403)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

        class Server(http.server.ThreadingHTTPServer):
            daemon_threads = False

            def get_request(self):
                connection, address = super().get_request()
                with endpoint.lock:
                    endpoint.connections.add(connection)
                return connection, address

        self.server = Server(("127.0.0.1", port), Handler, bind_and_activate=False)
        self.server.server_bind()
        guard.reserve(self.server)
        self.server.server_activate()
        self.worker = threading.Thread(target=self.server.serve_forever)
        self.worker.start()

    def authorized(self):
        port = C.c_uint32(self.server.server_port)
        lost, rule = C.c_uint32(), Binding()
        for name, value in (("lost", lost), ("policy", rule)):
            key = C.c_uint32(0) if name == "lost" else port
            if self.lib.bpf_map_lookup_elem(self.maps[name], C.byref(key), C.byref(value)) != 0:
                return False
        return lost.value == 0 and rule.revision == 1 and rule.deadline > time.monotonic_ns()

    def revoke(self):
        with self.lock:
            self.active = False
            key = C.c_uint32(self.server.server_port)
            assert self.lib.bpf_map_delete_elem(self.maps["policy"], C.byref(key)) == 0

    def close_listener(self):
        if self.closed:
            return
        with self.lock:
            self.active = False
        self.release.set()
        self.effect_release.set()
        self.server.shutdown()
        with self.lock:
            connections = list(self.connections)
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError as error:
                if error.errno != errno.ENOTCONN:
                    raise
        self.server.server_close()
        self.worker.join(5)
        assert not self.worker.is_alive() and not self.connections
        for fd in self.maps.values():
            os.close(fd)
        self.upstream.close()
        self.closed = True


def mount(*arguments):
    subprocess.run(["mount", *arguments], check=True, capture_output=True, timeout=5)


def object_ids(guard):
    lib = guard.lib
    lib.bpf_obj_get_info_by_fd.argtypes = [C.c_int, C.c_void_p, C.POINTER(C.c_uint32)]
    result = []
    for name in ("tasks", "connections", "policy", "ports", "listeners", "owners", "lost", "upstreams"):
        info, size = (C.c_uint32 * 2)(), C.c_uint32(8)
        assert lib.bpf_obj_get_info_by_fd(guard.map_fd(name), C.byref(info), C.byref(size)) == 0
        result.append(info[1])
    return result


def assert_maps_released(ids):
    lib = C.CDLL("libbpf.so.1", use_errno=True)
    lib.bpf_map_get_fd_by_id.argtypes = [C.c_uint32]
    deadline = time.monotonic() + 5
    while ids:
        remaining = []
        for identity in ids:
            fd = lib.bpf_map_get_fd_by_id(identity)
            if fd >= 0:
                os.close(fd)
                remaining.append(identity)
            else:
                assert C.get_errno() == errno.ENOENT, C.get_errno()
        ids = remaining
        assert time.monotonic() < deadline, ("owned kernel maps survived", ids)
        if ids:
            time.sleep(0.01)


def owner():
    require_guest()
    object_path, task_fd, supervisor_fd, upstream_fd = sys.argv[2:6]
    os.unshare(os.CLONE_NEWNS)
    mount("--make-rprivate", "/")
    fault = sys.argv[6] if len(sys.argv) == 7 else None
    if fault == "lsm":
        mount("--bind", "/dev/null", "/sys/kernel/security/lsm")
        require_guest()
    elif fault == "btf":
        mount("--bind", "/dev/null", "/sys/kernel/btf/vmlinux")
    elif fault == "object":
        object_path = "/dev/null"
    elif fault == "hook":
        object_path = str(Path(object_path).with_name("binding.bpf.o"))
    mount("-t", "bpf", "-o", "mode=0700", "bpf", "/sys/fs/bpf")
    if fault is None:
        # Retain the protection namespace before this process enrolls itself.
        # The runtime-protection hook intentionally rejects new cross-process
        # /proc handles after enrollment.
        print(json.dumps({"namespace_ready": os.getpid()}), flush=True)
        assert json.loads(sys.stdin.readline()) == "namespace-attached"
        print(json.dumps({"attached": True}), flush=True)
    guard = LifecycleGuard(object_path)
    api(guard.lib)
    endpoint = None
    try:
        # No listener exists before all hooks load. Reserved-port denial exists
        # before listen; authority stays absent until the parent's enable step.
        for index, link in enumerate([guard.link, *guard.extra_links]):
            assert guard.lib.bpf_link__pin(link, f"/sys/fs/bpf/link{index}".encode()) == 0
        guard.put("tasks", C.c_int(int(task_fd)), Grant(1, 101, 201, 0), 1)
        own_fd = os.pidfd_open(os.getpid())
        try:
            guard.put("tasks", C.c_int(own_fd), Grant(2, 101, 201, 0), 1)
        finally:
            os.close(own_fd)
        upstream_socket = socket.socket(fileno=int(upstream_fd))
        upstream = SimpleNamespace(socket=upstream_socket, server_port=upstream_socket.getsockname()[1],
                                   server_address=upstream_socket.getsockname(), address_family=socket.AF_INET)
        upstream_port = upstream.server_port
        guard.publish(upstream, launch=2)
        upstream_socket.close()
        mount("-o", "remount,ro", "/sys/fs/bpf")
        endpoint = OwnedEndpoint(guard, upstream_port)
        port = C.c_uint32(endpoint.server.server_port)
        guard.put("lost", C.c_uint32(0), C.c_uint32(0), 1)
        guard.put("owners", C.c_int(int(supervisor_fd)), C.c_uint32(0), 1)
        guard.lib.bpf_map_freeze.argtypes = [C.c_int]
        for name in ("ports", "lost", "owners", "tasks"):
            assert guard.lib.bpf_map_freeze(guard.map_fd(name)) == 0, (name, C.get_errno())
        print(json.dumps({"ready": os.getpid(), "port": endpoint.server.server_port,
                          "maps": object_ids(guard)}), flush=True)
        for line in sys.stdin:
            action = json.loads(line)
            if action == "enable":
                with endpoint.lock:
                    assert endpoint.binding is None
                    endpoint.binding = guard.publish(endpoint.server)
                    endpoint.active = True
                result = {"enabled": True, "authority": {name: getattr(endpoint.binding, name) for name in
                          ("launch", "session", "run", "revision", "deadline")}}
            elif action == "revoke":
                endpoint.revoke()
                endpoint.close_listener()
                result = {"revoked": True}
            elif isinstance(action, dict) and action.get("op") == "recover":
                expected = {name: getattr(endpoint.binding, name) for name in
                            ("launch", "session", "run", "revision", "deadline")}
                result = {"recovered": False}
                if guard is not None and endpoint.closed and action["authority"] == expected and time.monotonic_ns() < expected["deadline"]:
                    lost, key = C.c_uint32(), C.c_uint32(endpoint.server.server_port)
                    assert guard.lib.bpf_map_lookup_elem(guard.map_fd("lost"), C.byref(C.c_uint32(0)), C.byref(lost)) == 0
                    if lost.value == 0:
                        seen = endpoint.seen
                        endpoint = OwnedEndpoint(guard, upstream_port, port=key.value)
                        endpoint.seen = seen
                        with endpoint.lock:
                            endpoint.binding = guard.publish(endpoint.server, **expected)
                            endpoint.active = True
                        result = {"recovered": True}
            elif action == "unlink":
                try:
                    os.unlink("/sys/fs/bpf/link0")
                except OSError as error:
                    result = {"errno": error.errno}
                else:
                    result = {"errno": 0}
            elif action == "map-cleanup":
                for name, key in (("ports", port), ("lost", C.c_uint32(0)),
                                  ("owners", C.c_int(int(supervisor_fd))), ("tasks", C.c_int(int(task_fd)))):
                    assert guard.lib.bpf_map_delete_elem(guard.map_fd(name), C.byref(key)) < 0
                    assert C.get_errno() == errno.EPERM, (name, C.get_errno())
                for name, key in (("ports", port), ("lost", C.c_uint32(0))):
                    assert guard.lib.bpf_map_update_elem(guard.map_fd(name), C.byref(key), C.byref(C.c_uint32(0)), 0) < 0
                    assert C.get_errno() == errno.EPERM
                guard.lib.bpf_link__fd.argtypes = [C.c_void_p]
                for link in [guard.link, *guard.extra_links]:
                    assert guard.lib.bpf_link_detach(guard.lib.bpf_link__fd(link)) == -errno.EOPNOTSUPP
                result = {"protected": True}
            elif action == "drop-loader-fds":
                # Keep only the maps the admission owner actually reads. No
                # link/program descriptor survives this loader cleanup.
                guard.close()
                guard = None
                result = {"closed": True}
            elif action == "pause":
                endpoint.waiting.clear()
                endpoint.release.clear()
                result = {"paused": True}
            elif action == "pause-effect":
                endpoint.effect_waiting.clear()
                endpoint.effect_release.clear()
                result = {"paused": True}
            elif action == "waiting-effect":
                assert endpoint.effect_waiting.wait(5)
                result = {"waiting": True}
            elif action == "release-effect":
                endpoint.effect_release.set()
                result = {"released": True}
            elif action == "waiting":
                assert endpoint.waiting.wait(5)
                result = {"waiting": True}
            elif action == "release":
                endpoint.release.set()
                result = {"released": True}
            elif action == "drop-privileges":
                libc = C.CDLL(None, use_errno=True)
                assert libc.prctl(38, 1, 0, 0, 0) == 0  # PR_SET_NO_NEW_PRIVS
                os.setgroups([])
                os.setgid(4020010)
                os.setuid(4020010)
                status = Path("/proc/self/status").read_text().splitlines()
                assert "CapEff:\t0000000000000000" in status
                assert "NoNewPrivs:\t1" in status
                remount = subprocess.run(["mount", "-o", "remount,rw", "/sys/fs/bpf"],
                                         capture_output=True, timeout=5)
                assert remount.returncode != 0
                result = {"remount_denied": True}
            elif action == "seen":
                result = {"count": len(endpoint.seen)}
            else:
                raise ValueError(action)
            print(json.dumps(result), flush=True)
    finally:
        # Normal cleanup stops the endpoint before releasing enforcement. Pins
        # live in this mount namespace; no explicit unpin/unmount is needed.
        if endpoint is not None:
            endpoint.close_listener()
        if guard is not None:
            guard.close()


def main():
    assert sys.argv[1] == "--disposable-vm"
    require_guest()
    sender = controller = supervisor = upstream = None
    task_fd = supervisor_fd = namespace = None
    report = {}
    try:
        sender = spawn()
        task_fd = os.pidfd_open(sender.pid)
        signal.pidfd_send_signal(task_fd, signal.SIGSTOP)
        pid, status = os.waitpid(sender.pid, os.WUNTRACED)
        assert pid == sender.pid and os.WIFSTOPPED(status)
        assert os.path.samefile(f"/proc/{pid}/exe", sys.executable)
        supervisor = subprocess.Popen(
            [sys.executable, "-c", 'import os,sys,json\nprint(json.dumps({"ready":os.getpid()}), flush=True)\nfor line in sys.stdin:\n action=json.loads(line)\n if action=="attest": print(json.dumps({"alive":os.getpid()}), flush=True)\n elif action=="exec": os.execl("/bin/sleep", "sleep", "120")\n else: raise ValueError(action)'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        assert receive(supervisor)["ready"] == supervisor.pid
        supervisor_fd = os.pidfd_open(supervisor.pid)
        upstream = Endpoint()
        # No connection has been published yet. Join every observer handler
        # before releasing its namespace reference during final cleanup.
        upstream.server.daemon_threads = False
        upstream_fd = upstream.server.fileno()
        for fault in ("lsm", "btf", "object", "hook"):
            refused = subprocess.run(
                [sys.executable, __file__, "--owner", sys.argv[2], str(task_fd), str(supervisor_fd), str(upstream_fd), fault],
                capture_output=True, text=True, pass_fds=(task_fd, supervisor_fd, upstream_fd), timeout=10)
            assert refused.returncode != 0 and not refused.stdout, (fault, refused)
        report["unsupported_startup"] = "missing LSM/BTF, invalid object and absent owner hook refuse before listener construction"
        controller = subprocess.Popen(
            [sys.executable, __file__, "--owner", sys.argv[2], str(task_fd), str(supervisor_fd), str(upstream_fd)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, pass_fds=(task_fd, supervisor_fd, upstream_fd))
        namespace_ready = receive(controller)
        assert namespace_ready["namespace_ready"] == controller.pid
        namespace = os.open(f"/proc/{controller.pid}/ns/mnt", os.O_RDONLY)
        assert request(controller, "namespace-attached")["attached"]
        ready = receive(controller)
        # The synthetic upstream also keeps the protection namespace alive
        # until its own listener/connections stop; no endpoint outlives its pins.
        # An exit before enrollment must not be missed and later authorized.
        # Require an actual owner response AFTER its task-storage registration
        # and hooks exist. Death after this response hits the installed latch.
        # Production uses the existing authenticated supervisor/receipt channel.
        assert request(supervisor, "attest")["alive"] == supervisor.pid
        signal.pidfd_send_signal(task_fd, signal.SIGCONT)
        request(sender, {"op": "open", "address": "127.0.0.1", "port": ready["port"]})
        assert not request(sender, {"op": "send"})["accepted"]
        report["before_enable"] = "runtime denied"
        enabled = request(controller, "enable")
        assert enabled["enabled"]
        assert request(sender, {"op": "send"})["accepted"]
        assert not request(sender, {"op": "fork"})["accepted"]
        report["positive_control"] = "runtime accepted; inherited helper denied"
        request(sender, {"op": "open", "address": "127.0.0.1", "port": upstream.server.server_port})
        assert not request(sender, {"op": "send"})["accepted"]
        report["upstream_binding"] = "only the broker owner can send; runtime cannot bypass request admission"
        request(sender, {"op": "open", "address": "127.0.0.1", "port": ready["port"]})
        assert request(sender, {"op": "send"})["accepted"]
        assert request(controller, "revoke")["revoked"]
        assert not request(sender, {"op": "send"})["accepted"]
        for key in enabled["authority"]:
            changed = dict(enabled["authority"])
            changed[key] += 1
            assert not request(controller, {"op": "recover", "authority": changed})["recovered"]
        assert request(controller, {"op": "recover", "authority": enabled["authority"]})["recovered"]
        assert not request(sender, {"op": "send"})["accepted"]
        request(sender, {"op": "open", "address": "127.0.0.1", "port": ready["port"]})
        assert request(sender, {"op": "send"})["accepted"]
        report["recovery"] = "only exact unexpired authority restored; old connections shut down"
        assert request(controller, "unlink")["errno"] == errno.EROFS
        report["privileged_cleanup"] = "unlink denied EROFS"
        assert request(controller, "map-cleanup")["protected"]
        report["map_and_link_cleanup"] = "frozen identity/reservation/loss maps reject deletion; all six links reject explicit detach"
        assert request(controller, "drop-privileges")["remount_denied"]
        report["unprivileged_owner"] = "cannot remount guard pins writable"
        started = threading.Event()
        def hostile_sends():
            for index in range(200):
                assert not request(sender, {"op": "fork"})["accepted"]
                if index == 5:
                    started.set()
        with ThreadPoolExecutor(max_workers=1) as executor:
            hostile = executor.submit(hostile_sends)
            assert started.wait(5)
            assert request(controller, "drop-loader-fds")["closed"]
            hostile.result(timeout=10)
        assert request(sender, {"op": "send"})["accepted"]
        assert not request(sender, {"op": "fork"})["accepted"]
        report["all_loader_fds_closed"] = "mount-owned pins retain links/maps; helper denied"
        assert request(controller, "seen")["count"] == 4
        if "--broker-crash" in sys.argv:
            assert supervisor.poll() is None
            report["broker_loss"] = "kill endpoint owner while supervisor and runtime remain authorized"
        else:
            barrier = "" if "--before-check" in sys.argv else "-effect"
            assert request(controller, "pause" + barrier)["paused"]
            assert request(sender, {"op": "queue"})["queued"]
            assert request(controller, "waiting" + barrier)["waiting"]
            if "--supervisor-exec" in sys.argv:
                supervisor.stdin.write(json.dumps("exec") + "\n")
                supervisor.stdin.flush()
                deadline = time.monotonic() + 5
                while not os.path.samefile(f"/proc/{supervisor.pid}/exe", "/bin/sleep"):
                    assert time.monotonic() < deadline, "supervisor exec timeout"
                    time.sleep(0.001)
                change = "supervisor_exec"
            else:
                supervisor.kill()
                assert supervisor.wait(5) == -signal.SIGKILL
                change = "supervisor_sigkill"
            assert request(controller, "release" + barrier)["released"]
            assert request(sender, {"op": "reply"})["status"] == 403
            assert request(controller, "seen")["count"] == 4
            assert not request(controller, {"op": "recover", "authority": enabled["authority"]})["recovered"]
            report["buffered_before_supervisor_death"] = "queued request refused; no synthetic upstream request"
            report["admission_race"] = "death before check" if not barrier else "death after check, before actual upstream send: kernel denied write"
            assert not request(sender, {"op": "send"})["accepted"]
            assert not request(sender, {"op": "fork"})["accepted"]
            report[change] = "kernel hook revokes runtime and inherited sockets; no recovery"
        assert len(upstream.seen) == 4
        if "--orderly-close" in sys.argv:
            controller.stdin.close()
            assert controller.wait(5) == 0
        else:
            controller.kill()
            assert controller.wait(5) == -signal.SIGKILL
        # Refusal is established by a real connect result, not a silent timeout.
        try:
            socket.create_connection(("127.0.0.1", ready["port"]), timeout=2)
        except ConnectionRefusedError:
            pass
        else:
            raise AssertionError("endpoint survived owner death")
        report["owner_close" if "--orderly-close" in sys.argv else "owner_sigkill"] = "endpoint connection refused"
        assert request(sender, {"op": "eof"})["closed"]
        assert not Path(f"/proc/{ready['ready']}").exists()
    finally:
        if sender is not None:
            if sender.poll() is None:
                sender.kill()
            stop(sender)
        if controller is not None:
            stop(controller)
        if supervisor is not None:
            stop(supervisor)
        if upstream is not None:
            upstream.close_listener()
        if task_fd is not None:
            os.close(task_fd)
        if supervisor_fd is not None:
            os.close(supervisor_fd)
        if namespace is not None:
            os.close(namespace)
    assert_maps_released(ready["maps"])
    report["cleanup"] = "children reaped; endpoint/accepted socket closed; all eight owned kernel maps absent"
    print(json.dumps({"verdict": "OWNERSHIP_COMPONENT_PASS_NOT_VERIFIED",
                      "kernel": os.uname().release, "results": report,
                      # Existing conformance vocabulary; this single component
                      # observation cannot certify the complete installed host.
                      "observations": {
                          "schema": "louiselm.conformance.observations/1",
                          "scope": "disposable_guest",
                          "checks": [{"name": "lifecycle", "control": "Allowed",
                                      "confined": {"Denied": "guard ownership/loss component: inherited sends and queued effects refused"}}],
                          "completed": True, "cleanup": "confirmed"}}, indent=2))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--owner":
        owner()
    else:
        main()
