"""Opt-in link-lifetime counterexamples, not a production lifecycle design."""
import ctypes as C
import errno
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace

from binding_probe import BindingGuard, Endpoint, Grant, open_connection, spawn, stop
from guard_probe import receive, request


def require_guest():
    assert os.geteuid() == 0
    assert subprocess.check_output(["systemd-detect-virt", "--vm"], text=True).strip() == "kvm"
    assert "bpf" in Path("/sys/kernel/security/lsm").read_text().strip().split(",")
    assert subprocess.check_output(["stat", "-f", "-c", "%T", "/sys/fs/bpf"], text=True).strip() == "bpf_fs"


def api(lib):
    for name, args in {
        "bpf_link__pin": [C.c_void_p, C.c_char_p],
        "bpf_obj_get": [C.c_char_p],
        "bpf_link_detach": [C.c_int],
    }.items():
        function = getattr(lib, name)
        function.restype, function.argtypes = C.c_int, args


def loader():
    require_guest()
    object_path, directory, listener_fd, task_fd = sys.argv[2:]
    guard = BindingGuard(object_path)
    api(guard.lib)
    listener = socket.socket(fileno=int(listener_fd))
    server = SimpleNamespace(socket=listener, server_port=listener.getsockname()[1],
                             address_family=listener.family,
                             server_address=listener.getsockname())
    try:
        guard.put("tasks", C.c_int(int(task_fd)), Grant(1, 101, 201, 0), 1)
        guard.publish(server)
        for index, link in enumerate([guard.link, *guard.extra_links]):
            assert guard.lib.bpf_link__pin(link, os.fsencode(f"{directory}/link{index}")) == 0
        print(json.dumps({"ready": os.getpid()}), flush=True)
        # The parent kills this process, rather than calling orderly close().
        sys.stdin.read()
    finally:
        listener.close()
        guard.close()


def main():
    assert sys.argv[1] == "--disposable-vm"
    require_guest()
    directory = Path(tempfile.mkdtemp(prefix="louiselm-lifetime-", dir="/sys/fs/bpf"))
    endpoint, sender, owner, task_fd = None, None, None, None
    links, report = [], {}
    lib = C.CDLL("libbpf.so.1", use_errno=True)
    api(lib)
    try:
        endpoint = Endpoint()
        sender = spawn()
        task_fd = os.pidfd_open(sender.pid)
        signal.pidfd_send_signal(task_fd, signal.SIGSTOP)
        pid, status = os.waitpid(sender.pid, os.WUNTRACED)
        assert pid == sender.pid and os.WIFSTOPPED(status)
        assert os.path.samefile(f"/proc/{pid}/exe", sys.executable)
        owner = subprocess.Popen(
            [sys.executable, __file__, "--loader", sys.argv[2], str(directory),
             str(endpoint.server.fileno()), str(task_fd)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
            pass_fds=(endpoint.server.fileno(), task_fd))
        assert receive(owner)["ready"] == owner.pid
        signal.pidfd_send_signal(task_fd, signal.SIGCONT)
        open_connection(sender, endpoint)
        assert request(sender, {"op": "send"})["accepted"]
        assert not request(sender, {"op": "fork"})["accepted"]
        report["positive_control"] = "runtime accepted; inherited helper EPERM"

        owner.kill()
        assert owner.wait(5) == -signal.SIGKILL
        assert request(sender, {"op": "send"})["accepted"]
        assert not request(sender, {"op": "fork"})["accepted"]
        report["loader_sigkill"] = "pins retain all three links and their maps; helper EPERM"
        report["authority_after_loader_death"] = "existing runtime grant remains valid until its deadline; no automatic revocation"

        # A Session cannot traverse the root-owned 0700 pin directory.
        hostile = subprocess.run(
            [sys.executable, "-c", "import os,sys; os.unlink(sys.argv[1])", str(directory / "link0")],
            user=4020000, group=4020000, extra_groups=[], capture_output=True, text=True, timeout=5)
        assert hostile.returncode != 0 and "PermissionError" in hostile.stderr
        report["session_unlink"] = "EACCES"

        for index in range(3):
            fd = lib.bpf_obj_get(os.fsencode(directory / f"link{index}"))
            assert fd >= 0, C.get_errno()
            links.append(fd)
        for pin in directory.iterdir():
            pin.unlink()
        assert not request(sender, {"op": "fork"})["accepted"]
        report["pins_removed_with_retained_fds"] = "helper EPERM"

        # Repin the enforcement link: an explicit detach syscall is distinct
        # from dropping its final reference. This kernel refuses the former.
        lib.bpf_obj_pin.restype = C.c_int
        lib.bpf_obj_pin.argtypes = [C.c_int, C.c_char_p]
        assert lib.bpf_obj_pin(links[0], os.fsencode(directory / "retained")) == 0
        assert lib.bpf_link_detach(links[0]) == -errno.EOPNOTSUPP, C.get_errno()
        assert not request(sender, {"op": "fork"})["accepted"]
        report["privileged_force_detach_with_pin_and_fd"] = "EOPNOTSUPP; helper still EPERM"
        os.close(links.pop(0))
        assert not request(sender, {"op": "fork"})["accepted"]
        report["pin_is_last_send_link_reference"] = "helper EPERM"
        before = len(endpoint.seen)
        (directory / "retained").unlink()
        deadline = time.monotonic() + 5
        while not request(sender, {"op": "fork"})["accepted"]:
            # Kernel final-reference destruction is deferred. EPERM while that
            # completes is not evidence that a vanished pin preserves a hook.
            assert time.monotonic() < deadline, "final-reference loss not reproduced"
            time.sleep(0.01)
        assert len(endpoint.seen) == before + 1
        report["final_send_link_reference_removed"] = "helper accepted: final-reference loss still fails open"
    finally:
        if sender is not None:
            if sender.poll() is None:
                sender.kill()  # Also handles a failure while SIGSTOP is active.
            stop(sender)
        if owner is not None:
            stop(owner)
        if endpoint is not None:
            endpoint.close_listener()
        if task_fd is not None:
            os.close(task_fd)
        for fd in links:
            os.close(fd)
        for pin in directory.iterdir():
            pin.unlink()
        directory.rmdir()
    report["cleanup"] = "children reaped; endpoint stopped; owned fds and pins released"
    print(json.dumps({"verdict": "LIFETIME_COUNTEREXAMPLES_NOT_VERIFIED",
                      "kernel": os.uname().release, "results": report}, indent=2))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--loader":
        loader()
    else:
        main()
