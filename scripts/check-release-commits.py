#!/usr/bin/env python3
"""Enforce non-release commits for non-plugin files outside excluded directories.

Release Please 17.3.0 exclude-paths matches directories only. Keep these files
in place; CI checks this convention before the release action can propose them.
"""

import json
from pathlib import Path
import re
import subprocess


NON_PLUGIN_FILES = {
    "package.json", "package-lock.json", "myst.yml", "firebase.json", ".gitlab-ci.yml",
    "QA_CERTIFICATION.md", "scripts/build-site", "scripts/build-demo.mjs",
    "scripts/check-public-artifact", "scripts/test-public-artifact.sh",
    "scripts/test-demo-config.mjs", "scripts/test-demo-layout.js",
    "scripts/install-capture-service", "scripts/launcher-vm",
    "scripts/test-launcher-vm.sh", "scripts/launcher-conformance",
    "scripts/test-skills-core", "scripts/test-skills-core-git-isolation",
    "scripts/acp-log-backup", "scripts/test-acp-log-backup.py",
    "scripts/agent-liveness-snapshot", "scripts/test-agent-liveness-snapshot.sh",
}


def validate(message, files, excluded):
    def excluded_directory(path):
        return any(path.startswith(directory + "/") for directory in excluded)

    root_files = [path for path in files if not excluded_directory(path)]
    if root_files and all(path in NON_PLUGIN_FILES for path in root_files):
        if not re.match(r"(?:build|chore|ci|docs|style|test)(?:\([^\n()]+\))?: ", message) or re.search(
            r"(?im)^BREAKING[ -]CHANGE:|^Release-As:", message
        ):
            raise ValueError("non-plugin-only tooling changes must use build/chore/ci/docs/style/test without breaking or Release-As trailers")


def main():
    config = json.loads(Path("release-please-config.json").read_text())
    excluded = config["packages"]["."]["exclude-paths"]
    baseline = config["bootstrap-sha"]
    commits = subprocess.check_output(["git", "rev-list", f"{baseline}..HEAD"], text=True).splitlines()
    for sha in commits:
        message = subprocess.check_output(["git", "show", "-s", "--format=%B", sha], text=True)
        files = subprocess.check_output(["git", "diff-tree", "--no-commit-id", "--name-only", "--first-parent", "-m", "-r", sha], text=True).splitlines()
        try:
            validate(message, files, excluded)
        except ValueError as error:
            raise SystemExit(f"{sha}: {error}") from error
    print(f"Release commit policy: {len(commits)} commits checked")


if __name__ == "__main__":
    main()
