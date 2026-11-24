#!/usr/bin/env python3
"""Offline checks for the stock Codex provisioning output; no downloads or Agent."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


class StockChainTests(unittest.TestCase):
    def test_check_requires_exact_pins_without_downloading_or_rebuilding(self):
        module = runpy.run_path(str(Path(__file__).with_name("fetch-stock-codex-chain")))
        main = module["main"]
        artifacts = {name: {**pin, "file": hashlib.sha256(b"file").hexdigest()}
                     for name, pin in module["ARTIFACTS"].items()}
        with tempfile.TemporaryDirectory() as root, \
                patch.dict(main.__globals__, ARTIFACTS=artifacts,
                           fetch=lambda _: self.fail("check must not download")), \
                patch("sys.argv", ["fetch-stock-codex-chain", "--check", root]):
            directory = Path(root)
            for name, pin in artifacts.items():
                path = directory / name
                path.write_bytes(b"file")
                path.chmod(pin["mode"])
            inventory = directory / "louiselm-discovery.json"
            inventory.write_bytes(main.__globals__["discovery_inventory"]())
            inventory.chmod(0o444)
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                main()
            self.assertEqual(set(json.loads(stdout.getvalue())), {*artifacts, inventory.name})
            for name in ["node", "codex-acp.js", "codex", "codex-code-mode-host", inventory.name]:
                path = directory / name
                original = path.read_bytes()
                mode = path.stat().st_mode & 0o777
                path.chmod(0o644)
                path.write_bytes(b"not the pinned artifact")
                path.chmod(mode)
                with self.subTest(name=name), self.assertRaisesRegex(SystemExit, "pinned file differs: " + name):
                    main()
                path.chmod(0o644)
                path.write_bytes(original)
                path.chmod(mode)
            executable = directory / "node"
            executable.chmod(0o4555)
            with self.assertRaisesRegex(SystemExit, "pinned file mode differs: node"):
                main()
            executable.chmod(0o755)
            with self.assertRaisesRegex(SystemExit, "pinned file mode differs: node"):
                main()
            executable.unlink()
            executable.symlink_to(directory / "codex")
            with self.assertRaisesRegex(SystemExit, "pinned file is missing or not regular: node"):
                main()

    def test_provisioning_measures_the_actual_scoped_inventory(self):
        module = runpy.run_path(str(Path(__file__).with_name("fetch-stock-codex-chain")))
        main = module["main"]
        inventory = module["discovery_inventory"]()
        record = json.loads(inventory)
        self.assertEqual(record["schema"], "louiselm.discovery.skill-roots/1")
        self.assertEqual(record["runtime_id"], "codex-0-156-1")
        self.assertEqual(record["executable_sha256"], module["ARTIFACTS"]["node"]["file"])
        self.assertEqual({(s["root"], s["path"]) for s in record["sources"]
                          if s["kind"].endswith("skills")}, {
            ("home", ".agents/skills"), ("home", "skills"),
            ("home", "skills/.system"), ("workspace", ".agents/skills"),
            ("workspace", ".codex/skills"),
        })
        self.assertFalse(any(".managed" in s["path"] or "builtin" in s["path"]
                             for s in record["sources"]))
        # Exercise the real writer with synthetic pinned artifacts only.
        artifacts = {name: {**pin,
                           "archive": hashlib.sha256(b"archive").hexdigest(),
                           "file": hashlib.sha256(b"file").hexdigest()}
                     for name, pin in module["ARTIFACTS"].items()}
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "runtime"
            stdout = io.StringIO()
            with patch.dict(main.__globals__, ARTIFACTS=artifacts,
                            fetch=lambda _: b"archive", extract=lambda *_: b"file"), \
                    patch("sys.argv", ["fetch-stock-codex-chain", str(output)]), \
                    contextlib.redirect_stdout(stdout):
                main()
            digests = json.loads(stdout.getvalue())
            self.assertEqual(set(digests), {*artifacts, "louiselm-discovery.json"})
            for name, digest in digests.items():
                path = output / name
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), digest)
                self.assertEqual(path.stat().st_mode & 0o222, 0)


if __name__ == "__main__":
    unittest.main()
