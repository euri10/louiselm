#!/usr/bin/env python3
"""Offline capture package and immutable-upload boundary tests."""

import copy
import hashlib
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
import sys

sys.dont_write_bytecode = True
import capture_release as release


class CaptureAssets(unittest.TestCase):
    def test_archive_identity_integrity_and_retry_are_exact(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "binary"
            binary.write_bytes(b"fixture binary bytes")
            metadata = dict(component="capture", version="0.2.0", interfaces=dict(capture=1, run=1, attention=1, receiver=1))
            files = release.package(binary, root / "output", "a" * 40, "0.2.0", metadata)
            original = {path.name: path.read_bytes() for path in files}
            release.package(binary, root / "output", "a" * 40, "0.2.0", metadata)
            self.assertEqual(original, {path.name: path.read_bytes() for path in files})
            with tarfile.open(files[0]) as archive:
                self.assertEqual(archive.getnames(), ["louiselm-capture", "metadata.json"])
                self.assertEqual(archive.extractfile("louiselm-capture").read(), binary.read_bytes())
                record = json.load(archive.extractfile("metadata.json"))
                self.assertEqual(record["source_commit"], "a" * 40)
                self.assertEqual(record["binary_sha256"], hashlib.sha256(binary.read_bytes()).hexdigest())
            draft = dict(id=7, tag_name="capture-v0.2.0", draft=True, assets=[])
            writes = []

            def upload(repo, tag, path):
                writes.append(path.name)
                draft["assets"].append(dict(name=path.name, state="uploaded", digest="sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()))

            release.upload_missing("fixture/repo", draft, files, lambda _: draft, upload)
            self.assertEqual(len(writes), 3)
            draft["draft"] = False
            release.upload_missing("fixture/repo", draft, files, lambda _: draft, upload)
            self.assertEqual(len(writes), 3)
            for mutation in ("digest", "extra", "missing", "state"):
                damaged = copy.deepcopy(draft)
                if mutation == "digest": damaged["assets"][0]["digest"] = "sha256:wrong"
                if mutation == "extra": damaged["assets"].append(dict(name="surprise"))
                if mutation == "missing": damaged["assets"].pop()
                if mutation == "state": damaged["assets"][0]["state"] = "starter"
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    release.upload_missing("fixture/repo", damaged, files, lambda _: damaged, upload)
                self.assertEqual(len(writes), 3)
            with self.assertRaisesRegex(ValueError, "wrong identity"):
                release.package(binary, root / "rejected", "a" * 40, "0.3.0", metadata)
            self.assertFalse((root / "rejected").exists())


if __name__ == "__main__":
    unittest.main()
