#!/usr/bin/env python3
"""Check exact results and the campaign's predeclared gain/noise thresholds."""
import json
from pathlib import Path
import sys


baseline = json.loads(Path(sys.argv[1]).read_text())
candidate = json.loads(Path(sys.argv[2]).read_text())
targets = set(sys.argv[3:])
assert targets, "name at least one targeted broad query"
for key in ("cpu", "platform", "python", "nvim", "sqlite_cli", "sqlite_embedded", "sqlite_executable"):
    assert baseline["environment"][key] == candidate["environment"][key], (key, "environment changed")
assert baseline["datasets"].keys() == candidate["datasets"].keys()
errors, results = [], {}
for dataset, before in baseline["datasets"].items():
    after = candidate["datasets"][dataset]
    assert before["shape"] == after["shape"]
    assert before["database_bytes"] == after["database_bytes"]
    assert before["queries"].keys() == after["queries"].keys()
    assert targets <= before["queries"].keys()
    results[dataset] = {}
    for name, old in before["queries"].items():
        new = after["queries"][name]
        assert old["result_sha256"] == new["result_sha256"], (dataset, name, "result changed")
        assert old["bytes"] == new["bytes"], (dataset, name, "output size changed")
        prior, current = old["api_ms"], new["api_ms"]
        spread = prior["max"] - prior["min"]
        gain = prior["median"] - current["median"]
        required = max(prior["median"] * 0.10, spread, 25)
        tolerated = max(prior["median"] * 0.10, spread, 5)
        results[dataset][name] = {
            "before_ms": prior["median"], "after_ms": current["median"],
            "before_spread_ms": spread, "after_spread_ms": current["max"] - current["min"],
            "gain_ms": gain, "gain_percent": 100 * gain / prior["median"],
            "required_gain_ms": required, "regression_tolerance_ms": tolerated,
        }
        if gain < -tolerated:
            errors.append(f"{dataset}/{name}: regression exceeds noise tolerance")
        if dataset == "synthetic-251800" and name in targets and gain <= required:
            errors.append(f"{dataset}/{name}: gain below declared threshold")
print(json.dumps({"comparisons": results, "errors": errors}, indent=2, sort_keys=True))
sys.exit(bool(errors))
