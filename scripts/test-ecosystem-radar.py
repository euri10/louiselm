#!/usr/bin/env python3
"""Exercise the ecosystem-radar command using only captured JSON fixtures."""

import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "docs/workflows/fixtures"
COMMAND = [sys.executable, str(ROOT / "scripts/ecosystem-radar.py")]


def invoke(command, payload):
    return subprocess.run(
        [*COMMAND, command], input=json.dumps(payload), text=True, capture_output=True, check=False
    )


def main():
    receipt = json.loads((FIXTURES / "receipt-v1.json").read_text())
    receipt_result = invoke("validate", receipt)
    assert receipt_result.returncode == 0, receipt_result.stderr
    assert json.loads(receipt_result.stdout) == receipt

    bead_receipt = json.loads(json.dumps(receipt))
    bead_receipt["origin"] = {"kind": "bead", "id": "louiselm-sooa"}
    bead_receipt["chain"][0] = {"kind": "origin_bead", "bead_id": "louiselm-sooa"}
    result = invoke("validate", bead_receipt)
    assert result.returncode == 0, result.stderr

    unknown_author_receipt = json.loads(json.dumps(receipt))
    unknown_author_receipt["origin"]["author"]["handle"] = None
    result = invoke("validate", unknown_author_receipt)
    assert result.returncode == 0, result.stderr

    cases = json.loads((FIXTURES / "routing-cases-v1.json").read_text())
    assert cases["schema"] == "louiselm.ecosystem-radar-routing-cases/v1"
    for case in cases["cases"]:
        result = invoke("route", case["input"])
        assert result.returncode == 0, f"{case['id']}: {result.stderr}"
        assert json.loads(result.stdout) == case["expected"], case["id"]

    malformed_input = {
        "schema": "louiselm.ecosystem-radar-routing-input/v1",
        "candidates": [
            {
                "id": "untrusted",
                "relevance": "relevant",
                "conflicts": [],
                "matched_bead_ids": [],
                "requested_disposition": "linked_bead",
            }
        ],
    }
    result = invoke("route", malformed_input)
    assert result.returncode != 0 and "must contain only" in result.stderr

    malformed_type = {
        "schema": "louiselm.ecosystem-radar-routing-input/v1",
        "candidates": [
            {"id": "untrusted", "relevance": [], "conflicts": [], "matched_bead_ids": []}
        ],
    }
    result = invoke("route", malformed_type)
    assert result.returncode != 0 and "invalid relevance" in result.stderr
    assert "Traceback" not in result.stderr

    malformed_receipt = json.loads(json.dumps(receipt))
    malformed_receipt["candidates"][0]["disposition"] = "act"
    result = invoke("validate", malformed_receipt)
    assert result.returncode != 0 and "invalid disposition" in result.stderr

    malformed_date = json.loads(json.dumps(receipt))
    malformed_date["captured_at"] = "not-a-date"
    result = invoke("validate", malformed_date)
    assert result.returncode != 0 and "captured_at must be an ISO date" in result.stderr

    print(f"ecosystem radar: {len(cases['cases'])} command routing cases and receipt validation passed")


if __name__ == "__main__":
    main()
