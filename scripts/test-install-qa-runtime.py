#!/usr/bin/env python3
"""Offline source checks and append-only metadata tests; no administrator effects."""
import copy
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("install_qa_runtime", ROOT / "scripts/install-qa-runtime.py")
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


def proposal(root):
    runtime = root / "runtime"
    runtime.mkdir(mode=0o700)
    executable = b"offline byte fixture, not an Agent admission proof"
    identity = "qa-" + "a" * 24
    inventory = json.dumps({"runtime_id": identity, "executable_sha256": publisher.sha(executable)}).encode()
    (runtime / publisher.EXECUTABLE).write_bytes(executable)
    (runtime / publisher.INVENTORY).write_bytes(inventory)
    value = {"schema": "louiselm.qa.runtime-proposal/1", "state": "proposed", "release_id": "sha256:" + "a" * 64,
             "source_commit": "b" * 40, "agent": {"id": identity, "provider": "offline-test", "runtime_id": identity,
             "arguments": [], "environment": {}, "tool_integration": "louiselm.test-tool-integration/1"},
             "runtime": {"id": identity, "root": str(publisher.RUNTIMES / ("a" * 64)),
                         "executable": publisher.EXECUTABLE, "executable_sha256": publisher.sha(executable),
                         "adapters": [{"path": publisher.INVENTORY, "sha256": publisher.sha(inventory)}],
                         "version": "fixture", "origin": "release:sha256:" + "a" * 64},
             "envelope": {"id": "desktop-qa-denied", "network": "denied",
                          "description": "Explicit operator-only offline desktop conformance QA"},
             "inventory_digest": "sha256:" + publisher.sha(inventory)}
    data = json.dumps(value).encode()
    (root / "proposal.json").write_bytes(data)
    return value, "sha256:" + publisher.sha(data)


class Publisher(unittest.TestCase):
    def test_measured_source_refuses_substitution_and_widening_without_writes(self):
        with tempfile.TemporaryDirectory(prefix="louiselm-qa-publisher-", dir=Path.home()) as temporary:
            root = Path(temporary)
            value, reviewed = proposal(root)
            parsed, target, _, _ = publisher.measured_proposal(root, reviewed, os.getuid())
            self.assertEqual(parsed, value)
            self.assertEqual(target, publisher.RUNTIMES / ("a" * 64))
            for mutate in (lambda v: v["agent"]["arguments"].append("--override"),
                           lambda v: v["agent"]["environment"].update({"TOKEN": "must-never-be-logged"}),
                           lambda v: v["envelope"].update({"network": "brokered"}),
                           lambda v: v["runtime"].update({"root": "/etc"})):
                changed = copy.deepcopy(value)
                mutate(changed)
                data = json.dumps(changed).encode()
                (root / "proposal.json").write_bytes(data)
                with self.assertRaises(ValueError):
                    publisher.measured_proposal(root, "sha256:" + publisher.sha(data), os.getuid())
            (root / "proposal.json").write_bytes(json.dumps(value).encode())
            (root / "runtime" / publisher.EXECUTABLE).write_bytes(b"changed")
            with self.assertRaises(ValueError):
                publisher.measured_proposal(root, reviewed, os.getuid())

    def test_append_preserves_every_old_entry_and_rejects_conflicts_or_concurrent_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "agents.json"
            original = json.dumps({"schema": "louiselm.launch.registry/1", "entries": [{"id": "old", "unchanged": [1, 2]}]}).encode()
            path.write_bytes(original)
            entry = {"id": "qa", "fixed": True}
            publisher.append(path, entry, original)
            current = path.read_bytes()
            self.assertEqual(json.loads(current)["entries"], [{"id": "old", "unchanged": [1, 2]}, entry])
            publisher.append(path, entry, current)
            self.assertEqual(path.read_bytes(), current)
            with self.assertRaises(ValueError):
                publisher.append(path, {"id": "qa", "fixed": False}, current)
            with self.assertRaises(ValueError):
                publisher.append(path, {"id": "next"}, original)
            self.assertEqual(path.read_bytes(), current)

    def test_descriptor_reads_refuse_linked_files_and_symlink_ancestors(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data = root / "data"
            data.mkdir()
            (data / "record").write_bytes(b"fixed")
            (root / "linked").symlink_to(data, target_is_directory=True)
            with self.assertRaises(OSError):
                publisher.read_file(root / "linked/record", 100)
            os.link(data / "record", root / "alias")
            with self.assertRaises(ValueError):
                publisher.read_file(data / "record", 100)


if __name__ == "__main__":
    unittest.main()
