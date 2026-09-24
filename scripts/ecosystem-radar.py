#!/usr/bin/env python3
"""Route reviewed ecosystem-radar fields and validate v1 receipts."""

import json
import sys
from datetime import date


ROUTING_SCHEMA = "louiselm.ecosystem-radar-routing-input/v1"
RECEIPT_SCHEMA = "louiselm.ecosystem-radar/v1"
DISPOSITIONS = {"dismiss", "watch", "linked_bead"}
ASSESSMENT_DIMENSIONS = {"workflow_fit", "maturity", "security", "novelty"}


def read_json(stream):
    try:
        return json.load(stream)
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid JSON: {exc}") from exc


def require(condition, message):
    if not condition:
        raise ValueError(message)


def strings(value, label, allow_empty=True):
    require(isinstance(value, list), f"{label} must be an array")
    require(allow_empty or value, f"{label} must not be empty")
    require(all(isinstance(item, str) and item.strip() for item in value), f"{label} must contain non-empty strings")
    return value


def route(data):
    require(isinstance(data, dict), "routing input must be an object")
    require(data.get("schema") == ROUTING_SCHEMA, f"schema must be {ROUTING_SCHEMA}")
    require(set(data) <= {"schema", "source_text_untrusted", "candidates"}, "routing input has unknown fields")
    source_text = data.get("source_text_untrusted", "")
    require(isinstance(source_text, str), "source_text_untrusted must be a string")
    candidates = data.get("candidates")
    require(isinstance(candidates, list), "candidates must be an array")

    decisions = []
    seen = set()
    for candidate in candidates:
        require(isinstance(candidate, dict), "each candidate must be an object")
        require(
            set(candidate) == {"id", "relevance", "conflicts", "matched_bead_ids"},
            "candidate must contain only id, relevance, conflicts, and matched_bead_ids",
        )
        candidate_id = candidate["id"]
        require(isinstance(candidate_id, str) and candidate_id.strip(), "candidate id must be a non-empty string")
        require(candidate_id not in seen, f"duplicate candidate id: {candidate_id}")
        seen.add(candidate_id)
        relevance = candidate["relevance"]
        require(
            isinstance(relevance, str) and relevance in {"relevant", "not_relevant"},
            f"invalid relevance for {candidate_id}",
        )
        conflicts = strings(candidate["conflicts"], f"{candidate_id}.conflicts")
        bead_ids = strings(candidate["matched_bead_ids"], f"{candidate_id}.matched_bead_ids")

        if relevance == "not_relevant":
            disposition, linked = "dismiss", []
        elif conflicts:
            disposition, linked = "watch", []
        elif bead_ids:
            disposition, linked = "linked_bead", sorted(set(bead_ids))
        else:
            disposition, linked = "watch", []
        decisions.append(
            {"candidate_id": candidate_id, "disposition": disposition, "linked_bead_ids": linked}
        )

    return {
        "schema": "louiselm.ecosystem-radar-routing-result/v1",
        "pass_disposition": "dismiss" if not decisions else "review",
        "decisions": decisions,
    }


def validate_receipt(receipt):
    require(isinstance(receipt, dict), "receipt must be an object")
    require(receipt.get("schema") == RECEIPT_SCHEMA, f"schema must be {RECEIPT_SCHEMA}")
    required = {
        "schema", "captured_at", "origin", "chain", "candidates", "history_search",
        "unresolved", "created_bead_ids", "linked_bead_ids", "trust",
    }
    require(set(receipt) == required, "receipt has missing or unknown top-level fields")
    captured_at = receipt["captured_at"]
    require(isinstance(captured_at, str), "captured_at must be an ISO date")
    try:
        date.fromisoformat(captured_at)
    except ValueError as exc:
        raise ValueError("captured_at must be an ISO date") from exc

    origin = receipt["origin"]
    require(isinstance(origin, dict), "origin must be an object")
    kind = origin.get("kind")
    if kind == "public_issue":
        require(isinstance(origin.get("url"), str) and origin["url"].startswith("https://"), "public_issue origin.url must be HTTPS")
        author = origin.get("author")
        require(isinstance(author, dict), "public_issue origin.author must be an object")
        require("handle" in author, "public_issue origin.author.handle must be present; use null when unavailable")
        handle = author["handle"]
        require(handle is None or (isinstance(handle, str) and handle.strip()),
                "public_issue origin.author.handle must be a non-empty string or null")
    elif kind == "bead":
        require(isinstance(origin.get("id"), str) and origin["id"].strip(), "bead origin.id must be a Bead ID")
    else:
        raise ValueError("origin.kind must be public_issue or bead")

    chain = receipt["chain"]
    require(isinstance(chain, list) and chain, "chain must be a non-empty array")
    for link in chain:
        require(isinstance(link, dict) and isinstance(link.get("kind"), str), "each chain item needs a kind")

    candidates = receipt["candidates"]
    require(isinstance(candidates, list), "candidates must be an array")
    candidate_ids = set()
    for candidate in candidates:
        require(isinstance(candidate, dict), "each candidate must be an object")
        candidate_id = candidate.get("id")
        require(isinstance(candidate_id, str) and candidate_id.strip(), "candidate id must be a non-empty string")
        require(candidate_id not in candidate_ids, f"duplicate candidate id: {candidate_id}")
        candidate_ids.add(candidate_id)
        evidence = candidate.get("evidence")
        require(isinstance(evidence, list) and evidence, f"{candidate_id}.evidence must not be empty")
        for item in evidence:
            require(
                isinstance(item, dict)
                and isinstance(item.get("url"), str)
                and item["url"].startswith("https://")
                and isinstance(item.get("claim"), str)
                and item["claim"].strip(),
                f"{candidate_id} evidence requires an HTTPS URL and claim",
            )
        assessment = candidate.get("assessment")
        require(isinstance(assessment, dict) and set(assessment) == ASSESSMENT_DIMENSIONS,
                f"{candidate_id}.assessment must cover all four dimensions")
        for dimension, value in assessment.items():
            require(
                isinstance(value, dict)
                and isinstance(value.get("rating"), str)
                and isinstance(value.get("reason"), str)
                and value["reason"].strip(),
                f"{candidate_id}.{dimension} requires a rating and reason",
            )
        confidence = candidate.get("confidence")
        require(
            isinstance(confidence, dict)
            and isinstance(confidence.get("rating"), str)
            and isinstance(confidence.get("reason"), str)
            and confidence["reason"].strip(),
            f"{candidate_id}.confidence requires a rating and reason",
        )
        strings(candidate.get("risks"), f"{candidate_id}.risks")
        disposition = candidate.get("disposition")
        require(
            isinstance(disposition, str) and disposition in DISPOSITIONS,
            f"invalid disposition for {candidate_id}",
        )
        bead_ids = strings(candidate.get("linked_bead_ids"), f"{candidate_id}.linked_bead_ids")
        require(
            bool(bead_ids) == (disposition == "linked_bead"),
            f"{candidate_id} must link Beads exactly when disposition is linked_bead",
        )

    strings(receipt["created_bead_ids"], "created_bead_ids")
    strings(receipt["linked_bead_ids"], "linked_bead_ids")
    require(isinstance(receipt["history_search"], dict), "history_search must be an object")
    strings(receipt["history_search"].get("queries"), "history_search.queries", allow_empty=False)
    strings(receipt["history_search"].get("matches"), "history_search.matches")
    strings(receipt["unresolved"], "unresolved")
    trust = receipt["trust"]
    require(
        isinstance(trust, dict)
        and trust.get("external_text_is_untrusted_data") is True
        and trust.get("may_install_execute_approve_or_adopt") is False,
        "trust must mark external text untrusted and forbid install/execute/approval/adoption",
    )
    candidate_links = sorted({bead for c in candidates for bead in c["linked_bead_ids"]})
    require(candidate_links == sorted(set(receipt["linked_bead_ids"])), "top-level linked Beads must match candidate links")
    return receipt


def main(argv):
    if argv not in (["route"], ["validate"]):
        print("usage: ecosystem-radar.py {route|validate} < JSON", file=sys.stderr)
        return 2
    try:
        data = read_json(sys.stdin)
        result = route(data) if argv[0] == "route" else validate_receipt(data)
    except (OSError, ValueError) as exc:
        print(f"ecosystem radar: {exc}", file=sys.stderr)
        return 1
    json.dump(result, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
