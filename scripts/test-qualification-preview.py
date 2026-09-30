#!/usr/bin/env python3
"""Offline contract checks for the curated qualification preview."""

import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "docs/workflows/qualification-fixtures"
COMMAND = ROOT / "scripts/qualification-preview.py"


def invoke(bundle, manifest, mode="preview"):
    (bundle / "manifest.json").write_text(json.dumps(manifest))
    before = {str(path.relative_to(bundle)): path.read_bytes() for path in bundle.rglob("*") if path.is_file()}
    result = subprocess.run(
        [sys.executable, str(COMMAND), mode, str(bundle / "manifest.json")],
        cwd=bundle, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        text=True, capture_output=True, check=False,
    )
    after = {str(path.relative_to(bundle)): path.read_bytes() for path in bundle.rglob("*") if path.is_file()}
    assert before == after, "preview changed its input bundle"
    return result


def rejected(bundle, manifest, message):
    result = invoke(bundle, manifest)
    assert result.returncode == 1, result.stderr
    assert message in result.stderr, result.stderr
    assert not result.stdout


def main():
    with tempfile.TemporaryDirectory() as temp:
        bundle = Path(temp) / "bundle"
        shutil.copytree(FIXTURE, bundle)
        manifest = json.loads((bundle / "manifest.json").read_text())
        result = invoke(bundle, manifest)
        assert result.returncode == 0, result.stderr
        preview = json.loads(result.stdout)
        assert preview["schema"] == "louiselm.qualification-preview/v1"
        assert preview["launch"] == "blocked"
        assert len(preview["fixtures"]) == 3
        assert {f["kind"] for f in preview["fixtures"]} == {
            "bulk_reading", "mechanical_change", "reasoning"
        }
        assert preview["baseline"] == manifest["routes"]["baseline"]
        assert preview["candidate"] == manifest["routes"]["candidate"]
        assert preview["limits"] == manifest["limits"]
        assert preview["measurements"] == {"quota": "unknown", "prices": "unknown"}
        assert preview["disclosure"]["providers"] == manifest["disclosure"]["providers"]
        assert all("unsupported" in blocker for blocker in preview["blockers"])
        assert not preview["selected_history"]

        result = invoke(bundle, manifest, "validate")
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["valid"] is True

        check = manifest["fixtures"][1]["acceptance"]["commands"][0]
        original = (bundle / "mechanical/calculator.py").read_text()
        failed = subprocess.run(check, cwd=bundle, text=True, capture_output=True,
                                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}, check=False)
        assert failed.returncode != 0 and "AssertionError" in failed.stderr
        (bundle / "mechanical/calculator.py").write_text(original.replace("a - b", "a + b"))
        passed = subprocess.run(check, cwd=bundle, text=True, capture_output=True,
                                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}, check=False)
        assert passed.returncode == 0, passed.stderr
        (bundle / "mechanical/calculator.py").write_text(original)

        bad = copy.deepcopy(manifest)
        bad["surprise"] = True
        rejected(bundle, bad, "unknown")
        bad = copy.deepcopy(manifest)
        bad["routes"]["candidate"]["unexpected"] = True
        rejected(bundle, bad, "unknown")
        bad = copy.deepcopy(manifest)
        bad["routes"]["candidate"]["mode"] = "combined"
        rejected(bundle, bad, "mode")
        bad = copy.deepcopy(manifest)
        bad["limits"]["model_requests"] = 0
        rejected(bundle, bad, "model_requests")
        bad = copy.deepcopy(manifest)
        bad["limits"]["model_requests"] = 2 * len(bad["fixtures"]) - 1
        rejected(bundle, bad, "one request per fixture and arm")
        bad = copy.deepcopy(manifest)
        bad["limits"]["input_bytes"] = True
        rejected(bundle, bad, "input_bytes")
        bad = copy.deepcopy(manifest)
        bad["limits"]["input_bytes"] = 1
        rejected(bundle, bad, "input_bytes")
        bad = copy.deepcopy(manifest)
        bad["disclosure"]["providers"] = []
        rejected(bundle, bad, "providers")
        bad = copy.deepcopy(manifest)
        bad["isolation"]["deny_tracker_writes"] = False
        rejected(bundle, bad, "deny_tracker_writes")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["input"]["sha256"] = "0" * 64
        rejected(bundle, bad, "sha256 mismatch")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["input"]["path"] = "../private.txt"
        rejected(bundle, bad, "unsafe path")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["input"]["path"] = "/etc/passwd"
        rejected(bundle, bad, "unsafe path")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["input"]["path"] = "missing.txt"
        rejected(bundle, bad, "missing")
        (bundle / "linked.txt").symlink_to(bundle / "bulk/input.txt")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["input"]["path"] = "linked.txt"
        rejected(bundle, bad, "symlink")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["acceptance"]["reference_checks"] = []
        rejected(bundle, bad, "reference_checks")
        bad = copy.deepcopy(manifest)
        bad["fixtures"][0]["kind"] = []
        rejected(bundle, bad, "kind")

        selected = copy.deepcopy(manifest)
        selected["fixtures"][0]["provenance"] = {
            "kind": "selected_history", "reference": "operator selected synthetic example"
        }
        result = invoke(bundle, selected)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["selected_history"] == [selected["fixtures"][0]["id"]]
        (bundle / "unselected-private-history.txt").write_text("should not appear")
        result = invoke(bundle, manifest)
        assert result.returncode == 0, result.stderr
        assert "unselected-private-history" not in result.stdout

        source = bundle / manifest["fixtures"][0]["snapshots"]["source"][0]["path"]
        source.write_text("changed snapshot")
        rejected(bundle, manifest, "sha256 mismatch")
        source.unlink()
        rejected(bundle, manifest, "missing selected file")

        (bundle / "manifest.json").write_text('{"schema": "x", "schema": "y"}')
        result = subprocess.run(
            [sys.executable, str(COMMAND), "validate", str(bundle / "manifest.json")],
            text=True, capture_output=True, check=False,
        )
        assert result.returncode == 1 and "duplicate JSON key" in result.stderr

    print("qualification preview: valid, refusal, snapshot, and disclosure cases passed")


if __name__ == "__main__":
    main()
