#!/usr/bin/env python3
"""Offline checks for selected trial preparation; never launches a Model."""
import copy
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("qualification_run", ROOT / "scripts/qualification-run.py")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def main():
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        output = root / "not-created"
        refused = subprocess.run([sys.executable, str(ROOT / "scripts/qualification-run.py"),
                                  "run", str(root / "absent-selection"), "--output", str(output)],
                                 text=True, capture_output=True, check=False)
        assert refused.returncode == 1
        assert "qualification run refused or interrupted" in refused.stderr
        assert not output.exists(), "invalid selection must not stage or authorize work"
        bundle = root / "bundle"
        shutil.copytree(ROOT / "docs/workflows/qualification-fixtures", bundle)
        manifest = runner.preview.read_manifest(bundle / "manifest.json")
        runner.preview.validate(manifest, bundle)
        contents, prompts = runner.selected_inputs(manifest, bundle)
        assert len(contents) == 10 and len(prompts) == 3
        assert "Choose a meeting slot" in prompts[2]
        assert b"a - b" in contents["mechanical/calculator.py"]
        assert "manifest.json" not in contents
        (bundle / "unselected-secret").write_text("must not enter trial")
        assert "unselected-secret" not in runner.selected_inputs(manifest, bundle)[0]
        (bundle / "mechanical/calculator.py").write_text("changed selected content")
        try:
            runner.selected_inputs(manifest, bundle)
            raise AssertionError("changed input was admitted")
        except ValueError as error:
            assert "sha256 mismatch" in str(error)
        shutil.copyfile(ROOT / "docs/workflows/qualification-fixtures/mechanical/calculator.py", bundle / "mechanical/calculator.py")

        checkout = root / "fixture"
        runner.fixture_repository(contents, checkout)
        assert set(p.relative_to(checkout).as_posix() for p in checkout.rglob("*") if p.is_file() and ".git" not in p.parts) == set(contents)
        for name, value in contents.items():
            assert (checkout / name).read_bytes() == value
        assert (bundle / "mechanical/calculator.py").read_bytes() == contents["mechanical/calculator.py"]
        # Raw Git plumbing must not execute selected hooks or attribute filters.
        hostile = root / "hostile"
        runner.fixture_repository({".gitattributes": b"*.txt filter=hostile\n", "selected.txt": b"literal bytes"}, hostile)
        assert (hostile / "selected.txt").read_bytes() == b"literal bytes"

        plan = runner.verification_plan(manifest)
        assert plan["schema"] == "louiselm.workspace.verification-plan/1"
        assert plan["commands"][0]["argv"] == manifest["fixtures"][1]["acceptance"]["commands"][0]
        assert plan["commands"][0]["cwd"] == "."
        assert plan["commands"][0]["timeout_ms"] > 0
        pending = runner.pending_result(manifest, "trial", "policy", {"kind": "main", "id": "suite"}, "sha256:" + "a" * 64)
        assert all(c["candidate"] == "pending" for f in pending["report"]["fixtures"] for c in f["checks"])
        assert pending["observations"]["provider_requests"] is None
        assert "literal bytes" not in json.dumps(pending)

        selection = root / "selection.json"
        selection.write_text("{}")
        selection.chmod(0o644)
        try:
            runner.read_selection(selection)
            raise AssertionError("public selection was admitted")
        except ValueError as error:
            assert "0600" in str(error)
        selection.chmod(0o600)
        try:
            runner.read_selection(selection)
            raise AssertionError("missing authority was admitted")
        except ValueError as error:
            assert "missing" in str(error)
        print("qualification run preparation: passed (offline; no containment or paid-run acceptance)")


if __name__ == "__main__":
    main()
