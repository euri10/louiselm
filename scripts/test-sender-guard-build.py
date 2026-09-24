#!/usr/bin/env python3
"""Exercise the real Cargo build script in an offline, dependency-free fixture."""

import os
from pathlib import Path
import shutil
import runpy
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class GuardBuild(unittest.TestCase):
    def test_artifact_gate_rejects_a_missing_owner_hook(self):
        gate = runpy.run_path(str(ROOT / "scripts/test-sender-guard.py"))
        with tempfile.TemporaryDirectory(prefix="louiselm-guard-hook-") as directory:
            artifact = Path(directory) / "missing-hook.bpf.o"
            subprocess.run([
                "clang", "-target", "bpfel", "-D__TARGET_ARCH_x86", "-Downer_exit=missing_owner_exit",
                "-g", "-O2", "-Wall", "-Werror", "-I", "/usr/include/x86_64-linux-gnu",
                "-c", ROOT / "skills-core/src/launch_supervisor/sender_guard/lifecycle.bpf.c",
                "-o", artifact,
            ], check=True, timeout=30)
            with self.assertRaisesRegex(ValueError, "unexpected guard inventory"):
                gate["inspect"](artifact)

    def test_rebuilds_both_sources_and_reports_compiler_failures(self):
        with tempfile.TemporaryDirectory(prefix="louiselm-guard-build-") as directory:
            crate = Path(directory)
            source = crate / "src/launch_supervisor/sender_guard"
            shutil.copytree(ROOT / "skills-core/src/launch_supervisor/sender_guard", source)
            shutil.copyfile(ROOT / "skills-core/build.rs", crate / "build.rs")
            (crate / "Cargo.toml").write_text(
                '[package]\nname = "guard-build-fixture"\nversion = "0.0.0"\nedition = "2024"\n'
            )
            (crate / "src/main.rs").write_text(
                'use std::io::Write;\nfn main() { std::io::stdout().write_all('
                'include_bytes!(concat!(env!("OUT_DIR"), "/sender-guard.bpf.o"))).unwrap(); }\n'
            )
            environment = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TARGET_DIR=str(crate / "target"))

            def build():
                return subprocess.run(
                    ["cargo", "build", "--offline", "--quiet"], cwd=crate, env=environment,
                    capture_output=True, text=True, timeout=60,
                )

            def artifact():
                result = build()
                self.assertEqual(result.returncode, 0, result.stderr)
                return subprocess.check_output([crate / "target/debug/guard-build-fixture"], timeout=5)

            original = artifact()
            self.assertEqual(original[:6], b"\x7fELF\x02\x01")
            header = source / "binding.bpf.c"
            header_text = header.read_text()
            header.write_text(header_text.replace('= "GPL";', '= "Dual BSD/GPL";'))
            self.assertNotEqual(artifact(), original, "Cargo reused the guard after a header change")
            source_file = source / "lifecycle.bpf.c"
            source_text = source_file.read_text()
            source_file.write_text(source_text + '\n#error "guard-source-change"\n')
            failed = build()
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("guard-source-change", failed.stderr)
            self.assertIn("Sender guard compilation failed", failed.stderr)
            source_file.write_text(source_text)
            header.write_text(header_text)
            self.assertEqual(artifact(), original)

            # Cargo must rerun when compiler lookup changes, and a failed
            # compiler must not let an old object masquerade as a new build.
            fake_bin = crate / "bin"
            fake_bin.mkdir()
            compiler = fake_bin / "clang"
            compiler.write_text('#!/bin/sh\necho "guard-compiler-failure" >&2\nexit 42\n')
            compiler.chmod(0o755)
            environment["PATH"] = str(fake_bin) + os.pathsep + environment["PATH"]
            failed = build()
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("guard-compiler-failure", failed.stderr)
            self.assertIn("Sender guard compilation failed", failed.stderr)


if __name__ == "__main__":
    unittest.main()
