#!/usr/bin/env python3
"""Sync published release metadata from GitHub to GitLab, without rewriting refs."""

import argparse
import json
import re
import subprocess
import sys


def git(*args, failure):
    result = subprocess.run(["git", *args], text=True, capture_output=True)
    if result.returncode:
        raise ValueError(failure)
    return result.stdout.strip()


def ancestor(older, newer):
    result = subprocess.run(["git", "merge-base", "--is-ancestor", older, newer], capture_output=True)
    if result.returncode not in (0, 1):
        raise ValueError("cannot inspect release and GitLab ancestry")
    return result.returncode == 0


def synchronize(records, source, destination):
    if not isinstance(records, list) or len(records) > 4:
        raise ValueError("invalid release metadata")
    if not records:
        return
    tags = {}
    release_ids = set()
    shas = set()
    for record in records:
        if not isinstance(record, dict):
            raise ValueError("invalid release metadata")
        component = record.get("component")
        version = record.get("version")
        sha = record.get("sha")
        tag = record.get("tag")
        release_id = record.get("release_id")
        if (not isinstance(component, str) or not re.fullmatch(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*", component)
                or not isinstance(version, str) or not re.fullmatch(r"0\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version)
                or version == "0.0.0" or tag != f"{component}-v{version}"
                or not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{40}", sha)
                or type(release_id) is not int or release_id < 1 or release_id in release_ids or tag in tags):
            raise ValueError("invalid release metadata")
        release_ids.add(release_id)
        shas.add(sha)
        tags[tag] = None
    if len(shas) != 1:
        raise ValueError("release metadata must refer to one approved commit")
    release_sha = shas.pop()

    # Fetch the named source refs, not local tags: preserve annotated tag objects
    # as well as their peeled commit and do not trust a stale checkout.
    for tag in tags:
        git("fetch", "--no-tags", "--no-recurse-submodules", "--", source, f"refs/tags/{tag}",
            failure="cannot fetch source tag; check GitHub availability")
        oid = git("rev-parse", "--verify", "FETCH_HEAD", failure="cannot resolve source tag")
        commit = git("rev-parse", "--verify", "FETCH_HEAD^{commit}", failure="source tag does not resolve to a commit")
        if commit != release_sha:
            raise ValueError("source tag does not match approved release commit")
        tags[tag] = oid

    def destination_refs():
        lines = git("ls-remote", "--refs", "--", destination, "refs/heads/main",
                    *(f"refs/tags/{tag}" for tag in tags),
                    failure="cannot access GitLab; check SSH credentials, host key and connectivity").splitlines()
        return dict(line.split("\t", 1)[::-1] for line in lines)

    refs = destination_refs()
    for tag, oid in tags.items():
        existing = refs.get(f"refs/tags/{tag}")
        if existing and existing != oid:
            raise ValueError("conflicting tag on GitLab; never move a release tag")
    destination_sha = refs.get("refs/heads/main")
    if not destination_sha:
        raise ValueError("GitLab main is missing; inspect the destination before retrying")
    git("fetch", "--no-tags", "--no-recurse-submodules", "--", destination, "refs/heads/main",
        failure="cannot fetch GitLab main; check SSH credentials, host key and connectivity")
    fetched = git("rev-parse", "--verify", "FETCH_HEAD^{commit}", failure="cannot resolve GitLab main")
    if fetched != destination_sha:
        raise ValueError("GitLab main changed during inspection; retry")

    updates = []
    if ancestor(destination_sha, release_sha):
        if destination_sha != release_sha:
            updates.append(f"{release_sha}:refs/heads/main")
    elif not ancestor(release_sha, destination_sha):
        raise ValueError("GitLab main diverged from the approved release; reconcile history before retrying")
    if updates or any(f"refs/tags/{tag}" not in refs for tag in tags):
        updates.extend(f"{oid}:refs/tags/{tag}" for tag, oid in tags.items())
        git("push", "--atomic", "--no-follow-tags", "--no-signed", "--no-verify", "--recurse-submodules=no",
            "--", destination, *updates,
            failure="GitLab rejected the atomic update; inspect refs and credentials before retrying")
    final_refs = destination_refs()
    expected_main = release_sha if ancestor(destination_sha, release_sha) else destination_sha
    if (final_refs.get("refs/heads/main") != expected_main
            or any(final_refs.get(f"refs/tags/{tag}") != oid for tag, oid in tags.items())):
        raise ValueError("GitLab refs changed during synchronization; inspect and retry")
    print("GitLab synchronization complete: " + ", ".join(tags))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True)
    parser.add_argument("--destination", required=True)
    args = parser.parse_args()
    try:
        synchronize(json.load(sys.stdin), args.source, args.destination)
    except (ValueError, json.JSONDecodeError) as error:
        print(f"GitLab synchronization failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
