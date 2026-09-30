#!/usr/bin/env python3
"""Validate and preview selected qualification data without launching a Model."""

import hashlib
import json
import math
import re
import stat
import sys
from pathlib import Path


MANIFEST_SCHEMA = "louiselm.qualification-manifest/v1"
ISOLATION = {
    "private_snapshot", "deny_workspace_writes", "deny_tracker_writes",
    "deny_ambient_credentials", "deny_ambient_network", "provider_egress_only",
}
LIMITS = {"model_requests", "elapsed_seconds", "input_bytes", "output_bytes"}
KINDS = {"bulk_reading", "mechanical_change", "reasoning"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fields(value, required, label):
    require(isinstance(value, dict), f"{label} must be an object")
    unknown = set(value) - set(required)
    missing = set(required) - set(value)
    require(not unknown and not missing, f"{label} has unknown {sorted(unknown)} or missing {sorted(missing)} fields")


def nonempty(value, label):
    require(isinstance(value, str) and bool(value.strip()), f"{label} must be a non-empty string")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def invalid_constant(value):
    raise ValueError(f"invalid JSON constant: {value}")


def read_manifest(path):
    require(path.stat().st_size <= 1_000_000, "manifest exceeds 1 MB")
    try:
        return json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=unique_object,
            parse_constant=invalid_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid UTF-8 JSON manifest: {exc}") from exc


def selected_file(base, item, label, remaining):
    fields(item, {"path", "sha256"}, label)
    name = item["path"]
    nonempty(name, f"{label}.path")
    parts = name.split("/")
    require(not name.startswith("/") and "\\" not in name and all(
        part not in {"", ".", ".."} for part in parts
    ), f"{label}: unsafe path {name}")
    digest = item["sha256"]
    require(isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest),
            f"{label}.sha256 must be a lowercase SHA-256 digest")
    path = base
    for part in parts:
        path = path / part
        try:
            mode = path.lstat().st_mode
        except FileNotFoundError as exc:
            raise ValueError(f"{label}: missing selected file {name}") from exc
        require(not stat.S_ISLNK(mode), f"{label}: unsafe path {name} (symlink)")
    require(stat.S_ISREG(mode), f"{label}: selected path is not a regular file: {name}")
    require(path.stat().st_size <= remaining, f"{label}: selected input exceeds input_bytes")
    hasher = hashlib.sha256()
    size = 0
    chunks = []
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            size += len(chunk)
            require(size <= remaining, f"{label}: selected input exceeds input_bytes")
            hasher.update(chunk)
            chunks.append(chunk)
    require(hasher.hexdigest() == digest, f"{label}: sha256 mismatch for {name}")
    try:
        contents = b"".join(chunks).decode("utf-8")
    except UnicodeError as exc:
        raise ValueError(f"{label}: selected file is not UTF-8: {name}") from exc
    return name, size, contents


def validate(manifest, base):
    fields(manifest, {"schema", "routes", "limits", "disclosure", "isolation", "fixtures"}, "manifest")
    require(manifest["schema"] == MANIFEST_SCHEMA, f"schema must be {MANIFEST_SCHEMA}")
    routes = manifest["routes"]
    fields(routes, {"baseline", "candidate"}, "routes")
    for arm in ("baseline", "candidate"):
        route = routes[arm]
        fields(route, {"mode", "agent", "provider", "options"}, f"routes.{arm}")
        require(route["mode"] == "direct", f"routes.{arm}.mode must be direct in v1")
        for key in ("agent", "provider"):
            nonempty(route[key], f"routes.{arm}.{key}")
        options = route["options"]
        require(isinstance(options, dict), f"routes.{arm}.options must be an object")
        nonempty(options.get("model"), f"routes.{arm}.options.model")
        for key, value in options.items():
            nonempty(key, f"routes.{arm}.options key")
            require(isinstance(value, (str, int, float, bool))
                    and (not isinstance(value, float) or math.isfinite(value)),
                    f"routes.{arm}.options.{key} must be a finite scalar")
    require(routes["baseline"] != routes["candidate"], "baseline and candidate routes must differ")

    limits = manifest["limits"]
    fields(limits, LIMITS, "limits")
    for key in LIMITS:
        require(type(limits[key]) is int and limits[key] > 0, f"limits.{key} must be a positive integer")
    fields(manifest["isolation"], ISOLATION, "isolation")
    for key in ISOLATION:
        require(manifest["isolation"][key] is True, f"isolation.{key} must be true")
    disclosure = manifest["disclosure"]
    fields(disclosure, {"providers"}, "disclosure")
    providers = disclosure["providers"]
    require(isinstance(providers, list) and all(isinstance(p, str) for p in providers),
            "disclosure.providers must be an array of Provider names")
    require(sorted(providers) == sorted({route["provider"] for route in routes.values()}),
            "disclosure.providers must exactly name the baseline and candidate Providers")

    fixtures = manifest["fixtures"]
    require(isinstance(fixtures, list) and bool(fixtures), "fixtures must be a non-empty array")
    require(limits["model_requests"] >= 2 * len(fixtures),
            "limits.model_requests must allow one request per fixture and arm")
    seen_ids = set()
    kinds = set()
    selected_paths = set()
    selected_history = []
    total = 0
    for index, fixture in enumerate(fixtures):
        label = f"fixtures[{index}]"
        fields(fixture, {"id", "kind", "provenance", "input", "snapshots", "acceptance"}, label)
        nonempty(fixture["id"], f"{label}.id")
        require(fixture["id"] not in seen_ids, f"duplicate fixture id: {fixture['id']}")
        seen_ids.add(fixture["id"])
        kind = fixture["kind"]
        require(isinstance(kind, str) and kind in KINDS,
                f"{label}.kind must be one of {sorted(KINDS)}")
        kinds.add(kind)
        provenance = fixture["provenance"]
        fields(provenance, {"kind", "reference"}, f"{label}.provenance")
        require(isinstance(provenance["kind"], str)
                and provenance["kind"] in {"synthetic", "selected_history"},
                f"{label}.provenance.kind must be synthetic or selected_history")
        nonempty(provenance["reference"], f"{label}.provenance.reference")
        if provenance["kind"] == "selected_history":
            selected_history.append(fixture["id"])
        snapshots = fixture["snapshots"]
        fields(snapshots, {"source", "instructions"}, f"{label}.snapshots")
        for key in ("source", "instructions"):
            require(isinstance(snapshots[key], list) and bool(snapshots[key]),
                    f"{label}.snapshots.{key} must be a non-empty array")
        entries = [("input", fixture["input"])] + [
            (key, item) for key in ("source", "instructions") for item in snapshots[key]
        ]
        contents = {}
        fixture_paths = set()
        for key, item in entries:
            name, size, text = selected_file(base, item, f"{label}.{key}", limits["input_bytes"] - total)
            require(name not in fixture_paths, f"{label}: duplicate selected path {name}")
            fixture_paths.add(name)
            selected_paths.add(name)
            contents[name] = text
            total += size

        acceptance = fixture["acceptance"]
        fields(acceptance, {"reference_checks", "commands", "human_review"}, f"{label}.acceptance")
        checks = acceptance["reference_checks"]
        commands = acceptance["commands"]
        review = acceptance["human_review"]
        require(isinstance(checks, list), f"{label}.acceptance.reference_checks must be an array")
        require(isinstance(commands, list), f"{label}.acceptance.commands must be an array")
        require(review is None or (isinstance(review, str) and bool(review.strip())),
                f"{label}.acceptance.human_review must be null or a non-empty rubric")
        source_paths = {item["path"] for item in snapshots["source"]}
        for check in checks:
            fields(check, {"source_path", "answer_contains", "citation"}, f"{label}.reference_check")
            nonempty(check["source_path"], f"{label}.reference_check.source_path")
            require(check["source_path"] in source_paths, f"{label}: reference source_path is not selected")
            for key in ("answer_contains", "citation"):
                nonempty(check[key], f"{label}.reference_check.{key}")
                require(check[key] in contents[check["source_path"]],
                        f"{label}: reference {key} is absent from selected source")
        for command in commands:
            require(isinstance(command, list) and bool(command) and all(
                isinstance(arg, str) and bool(arg) and not any(ord(ch) < 32 for ch in arg)
                for arg in command
            ), f"{label}.acceptance command must be a non-empty argv array")
        require(kind != "bulk_reading" or checks, f"{label}: bulk_reading requires reference_checks")
        require(kind != "mechanical_change" or commands, f"{label}: mechanical_change requires commands")
        require(kind != "reasoning" or review, f"{label}: reasoning requires human_review")
    require(kinds == KINDS, f"fixtures must include {sorted(KINDS)}")
    return sorted(selected_paths), selected_history, total


def main(argv):
    if len(argv) != 2 or argv[0] not in {"validate", "preview"}:
        print("usage: qualification-preview.py {validate|preview} manifest.json", file=sys.stderr)
        return 2
    try:
        path = Path(argv[1])
        manifest = read_manifest(path)
        paths, history, input_size = validate(manifest, path.parent)
    except (OSError, ValueError) as exc:
        print(f"qualification preview: {exc}", file=sys.stderr)
        return 1
    if argv[0] == "validate":
        result = {"schema": "louiselm.qualification-validation/v1", "valid": True,
                  "selected_files": len(paths), "input_bytes": input_size}
    else:
        providers = manifest["disclosure"]["providers"]
        result = {
            "schema": "louiselm.qualification-preview/v1", "launch": "blocked",
            "baseline": manifest["routes"]["baseline"],
            "candidate": manifest["routes"]["candidate"],
            "limits": manifest["limits"], "fixtures": manifest["fixtures"],
            "selected_history": history,
            "disclosure": {"providers": providers,
                           "possible_files_by_provider": {provider: paths for provider in providers}},
            "isolation": manifest["isolation"],
            "measurements": {"quota": "unknown", "prices": "unknown"},
            "blockers": [f"unsupported: no qualification runtime proves {key}" for key in sorted(ISOLATION)]
            + ["unsupported: Agent options, Provider resolution, and acceptance command containment are unverified"],
        }
    json.dump(result, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
