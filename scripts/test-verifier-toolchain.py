#!/usr/bin/env python3
"""Offline ownership regression in a private filesystem; never host provisioning."""

import hashlib
import io
import os
from pathlib import Path
import resource
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest


ROOT = Path("/usr/lib/louiselm/verifier")
SOURCE = Path("/source/verifier-toolchain")


class VerifierOwnershipTests(unittest.TestCase):
    def setUp(self):
        self.addCleanup(shutil.rmtree, ROOT)
        ROOT.mkdir(mode=0o755)
        (ROOT / "input").write_bytes(b"measured fixture\n")
        (ROOT / "input").chmod(0o644)
        digest = hashlib.sha256((ROOT / "input").read_bytes()).hexdigest()
        (ROOT / "SHA256SUMS").write_text(f"{digest}  input\n")
        (ROOT / "SHA256SUMS").chmod(0o644)
        (ROOT / "directory").mkdir(mode=0o755)
        (ROOT / "internal-link").symlink_to("input")

    def check(self, expected):
        result = subprocess.run(["bash", str(SOURCE), "check"],
                                text=True, capture_output=True, timeout=5, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(expected, result.stderr)

    def test_safe_metadata_reaches_existing_link_check(self):
        # The minimal fixture intentionally has no Neovim: no fixture tool executes.
        self.check("Neovim link drift")

    def test_foreign_file_owner_refused_before_tools(self):
        os.chown(ROOT / "input", 1001, 0)
        self.check("untrusted toolchain ownership or permissions")

    def test_foreign_directory_group_refused_before_tools(self):
        os.chown(ROOT / "directory", 0, 1001)
        self.check("untrusted toolchain ownership or permissions")

    def test_foreign_link_owner_refused_before_tools(self):
        os.chown(ROOT / "internal-link", 1001, 1001, follow_symlinks=False)
        self.check("untrusted toolchain ownership or permissions")

    def test_group_writable_file_refused_before_tools(self):
        (ROOT / "input").chmod(0o664)
        self.check("untrusted toolchain ownership or permissions")

    def test_world_writable_root_refused_before_tools(self):
        ROOT.chmod(0o757)
        self.check("untrusted toolchain ownership or permissions")

    def test_world_writable_directory_refused_before_tools(self):
        (ROOT / "directory").chmod(0o777)
        self.check("untrusted toolchain ownership or permissions")

    def test_installer_extraction_does_not_inherit_upstream_ids(self):
        # l72qk: LuaLS carries foreign IDs; git archives carry group-write modes.
        # Exercise the provisioner's actual extraction commands, not copied flags.
        commands = [line.strip() for line in SOURCE.read_text().splitlines()
                    if line.strip().startswith("tar ") and '"$scratch/' in line]
        self.assertEqual(len(commands), 2)
        with tempfile.TemporaryDirectory() as name:
            scratch = Path(name)
            stage = scratch / "stage"
            for tool in ("nvim", "luals"):
                (stage / tool).mkdir(parents=True)
                with tarfile.open(scratch / f"{tool}.tar.gz", "w:gz") as archive:
                    directory = "upstream/directory" if tool == "nvim" else "directory"
                    member = tarfile.TarInfo(directory)
                    member.type = tarfile.DIRTYPE
                    member.uid = member.gid = 1001
                    member.mode = 0o775
                    archive.addfile(member)
                    member = tarfile.TarInfo(f"{directory}/input")
                    member.uid = member.gid = 1001
                    member.mode = 0o664
                    member.size = 7
                    archive.addfile(member, io.BytesIO(b"fixture"))
            result = subprocess.run(["bash", "-eu", "-o", "pipefail", "-c",
                                     "umask 022\n" + "\n".join(commands)], text=True, capture_output=True,
                                    env={"PATH": "/usr/bin:/bin", "scratch": str(scratch),
                                         "stage": str(stage)}, timeout=5, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            for tool in ("nvim", "luals"):
                for relative, mode in (("directory", 0o755), ("directory/input", 0o644)):
                    metadata = (stage / tool / relative).stat()
                    self.assertEqual((metadata.st_uid, metadata.st_gid), (0, 0))
                    self.assertEqual(metadata.st_mode & 0o7777, mode)


def limits():
    for kind, value in ((resource.RLIMIT_CPU, 60), (resource.RLIMIT_AS, 512 * 1024**2),
                        (resource.RLIMIT_NPROC, 64), (resource.RLIMIT_FSIZE, 64 * 1024**2)):
        resource.setrlimit(kind, (value, value))


def fingerprint():
    result = {}
    for path in [ROOT, *sorted(ROOT.rglob("*"))]:
        info = path.lstat()
        content = (os.readlink(path) if path.is_symlink() else
                   hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None)
        result[str(path.relative_to(ROOT))] = (info.st_mode, info.st_uid, info.st_gid,
                                              info.st_mtime_ns, content)
    return result


def installed_checks(mode):
    if mode == "writable":
        shutil.copytree("/toolchain-source", ROOT, symlinks=True)
    before = fingerprint()
    for uid in (0, 1000):
        for repeat in range(2):
            result = subprocess.run(["setpriv", f"--reuid={uid}", f"--regid={uid}",
                                     "--clear-groups", "bash", str(SOURCE), "check"],
                                    text=True, capture_output=True, timeout=20, check=False)
            assert result.returncode == 0, result.stderr[-2000:]
            after = fingerprint()
            changed = [name for name in before.keys() | after.keys()
                       if before.get(name) != after.get(name)]
            assert not changed, f"self-check changed measured tools: {sorted(changed)[:10]}"
            assert not list(Path("/tmp").glob("louiselm-verifier-check.*")), "check scratch leaked"
            print(f"PASS: {mode}, uid {uid}, check {repeat + 1}, unchanged tools", flush=True)


if __name__ == "__main__":
    if len(sys.argv) == 2 and sys.argv[1] in ("--inside", "--inside-writable", "--inside-readonly"):
        mounts = Path("/proc/self/mountinfo").read_text().splitlines()
        assert os.geteuid() == 0
        if sys.argv[1] == "--inside-readonly":
            assert any(line.split()[4] == "/usr" and "ro" in line.split()[5].split(",")
                       for line in mounts), "installed tools must be read-only"
        else:
            assert any(line.split()[4] == str(ROOT.parent) and " - tmpfs " in line
                       for line in mounts), "fixture must not reach the installed toolchain"
        if sys.argv[1] == "--inside":
            unittest.main(argv=[sys.argv[0]], verbosity=2)
        else:
            installed_checks(sys.argv[1].removeprefix("--inside-"))
    else:
        if sys.argv[1:] not in ([], ["--installed"]) or os.geteuid() != 0:
            raise SystemExit("run as root in the prepared launcher VM; optional --installed checks real tools")
        script = Path(__file__).resolve()
        for mode in (("writable", "readonly") if sys.argv[1:] else ("metadata",)):
            command = ["bwrap", "--unshare-pid", "--unshare-net", "--unshare-ipc",
                       "--unshare-uts", "--die-with-parent", "--new-session", "--clearenv",
                       "--cap-add", "CAP_CHOWN", "--cap-add", "CAP_FOWNER",
                       "--cap-add", "CAP_SETUID", "--cap-add", "CAP_SETGID"]
            for path in ("/usr", "/bin", "/sbin", "/lib", "/lib64"):
                command += ["--ro-bind", path, path]
            if mode != "metadata":
                command += ["--ro-bind", str(ROOT), "/toolchain-source",
                            "--dir", "/var", "--chmod", "755", "/var",
                            "--dir", "/var/lib", "--chmod", "755", "/var/lib",
                            "--ro-bind", "/var/lib/dpkg", "/var/lib/dpkg"]
            if mode != "readonly":
                command += ["--size", "1073741824", "--tmpfs", str(ROOT.parent),
                            "--chmod", "755", str(ROOT.parent)]
            command += ["--proc", "/proc", "--dev", "/dev", "--size", "33554432",
                        "--tmpfs", "/tmp", "--chmod", "1777", "/tmp",
                        "--dir", "/source", "--chmod", "755", "/source",
                        "--ro-bind", str(script), "/source/test.py",
                        "--ro-bind", str(script.with_name("verifier-toolchain")), str(SOURCE),
                        "--setenv", "PATH", "/usr/bin:/bin", "--setenv", "HOME", "/tmp",
                        "--setenv", "TMPDIR", "/tmp", "--chdir", "/tmp", "--", "python3",
                        "/source/test.py", "--inside" if mode == "metadata" else f"--inside-{mode}"]
            result = subprocess.run(command, env={"PATH": "/usr/bin:/bin"},
                                    preexec_fn=limits, timeout=120, check=False)
            if result.returncode:
                raise SystemExit(result.returncode)
