#!/usr/bin/env python3
"""Check privileged CI dispatch without executing Cargo or host sudo."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
RUNNER = ROOT / "scripts/test-skills-privileged"
SYSTEM = "launch_supervisor::system::"
INSTALLED = SYSTEM + "installed_tests::"

# The pre-split CI contract: gate, mask, filter, selection, execution prefix.
CORE = [
    ("TOOL_ISOLATION", "022", SYSTEM + "tool_integration_tests::privileged_measured_agent_owns_isolated_tool_lifecycle", "--exact", []),
    ("TOOL_GRANTS", "022", "launch_supervisor::lifecycle::tool_dispatch::tests::grant_tests::privileged_measured_helper_grant_execution_and_revocation", "--exact", []),
    ("BROKER_LAUNCH", "022", INSTALLED + "privileged_installed_broker_launch_and_effects", "--exact", []),
    ("BROKER_LAUNCH", "022", INSTALLED + "provider_credentials::privileged_installed_provider_credentials_stay_broker_side", "--exact", ["timeout", "120"]),
    ("BROKER_LAUNCH", "077", INSTALLED + "receipt_history::privileged_installed_receipt_history_survives_rotation_and_upgrade", "--exact", ["timeout", "120"]),
    ("BROKER_LAUNCH", "022", INSTALLED + "receipt_history::isolation::privileged_installed_history_failure_is_session_local", "--exact", ["timeout", "120"]),
    ("BROKER_LAUNCH", "022", INSTALLED + "receipt_history::revocation", "--test-threads=1", ["timeout", "180"]),
    ("BROKER_LAUNCH", "022", INSTALLED + "receipt_history::cleanup", "--test-threads=1", ["timeout", "180"]),
    ("BROKER_LAUNCH", "022", INSTALLED + "socket_activation::privileged_manager_listener_authenticates_only_broker_senders_across_restart", "--exact", ["timeout", "120"]),
    ("BROKER_LAUNCH", "022", INSTALLED + "verification::privileged_installed_exact_job_verification", "--exact", ["timeout", "180"]),
    ("CERTIFICATION", "022", INSTALLED + "certification::privileged_installed_certification_owns_probes_and_retains_exact_evidence", "--exact", ["timeout", "200", "unshare", "--net", "--"]),
]
MOUNT = ["timeout", "240", "unshare", "--mount", "--propagation", "private", "--"]
LIFECYCLE = [
    ("CONTROL_DAEMON", "022", INSTALLED + "daemon::" + scenario, "--exact", MOUNT)
    for scenario in ["privileged_activated_daemon_serves_launches_and_restart",
                     "state::privileged_activated_daemon_upgrades_and_adopts_state",
                     "inspection::privileged_activated_daemon_refuses_foreign_inspection"]
] + [
    ("BROKER_BEADS", "022", INSTALLED + "daemon::beads::privileged_installed_tracker_routes_only_approved_mutations", "--exact", ["env", "LOUISELM_TEST_BEADS_INSTALLER=" + str(ROOT / "scripts/install-broker-beads.py"), *MOUNT]),
] + [
    ("BROKER_LAUNCH", "022", INSTALLED + "privileged_installed_cold_resume_" + scenario, "--exact", ["timeout", "240"])
    for scenario in ["finite", "uncapped", "failed_load", "unavailable_balance"]
] + [
    ("BROKER_LAUNCH", "022", INSTALLED + "failures::privileged_installed_launch_failures_never_acknowledge_success", "--exact", []),
]

STUB = '''#!/usr/bin/env python3
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
if name == "cargo":
    if "--message-format=json" in sys.argv:
        print(json.dumps({"reason":"compiler-artifact", "target":{"name":"louiselm_skills"}, "profile":{"test":True}, "executable":os.environ["FIXTURE_LIBRARY"]}))
elif name == "library":
    assert "--list" in sys.argv
    if not os.environ.get("EMPTY_TEST_LIST"):
        print(sys.argv[1] + ": test")
elif name == "sudo":
    with open(os.environ["FIXTURE_CALLS"], "a") as stream:
        stream.write(json.dumps(sys.argv[1:]) + "\\n")
    if os.environ.get("FAIL_SCENARIO") in sys.argv:
        sys.exit(23)
else:
    raise AssertionError(name)
'''


class Dispatch(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in ["cargo", "library", "sudo"]:
            target = self.root / name
            target.write_text(STUB)
            target.chmod(0o755)
        self.calls = self.root / "calls.jsonl"
        self.env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ["PATH"],
                        FIXTURE_LIBRARY=str(self.root / "library"), FIXTURE_CALLS=str(self.calls))

    def run_group(self, *arguments, **env):
        return subprocess.run(["bash", str(RUNNER), *arguments], env=self.env | env,
                              text=True, capture_output=True)

    def recorded(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()] if self.calls.exists() else []

    def test_every_original_invocation_runs_once_with_its_authority_boundaries(self):
        for group in ["core", "lifecycle"]:
            result = self.run_group("--disposable-guest", group)
            self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.recorded()
        self.assertEqual(len(calls), 21)
        rust_calls = [call for call in calls if str(self.root / "library") in call]
        self.assertEqual(len(rust_calls), 20)
        for gate, mask, name, selection, prefix in CORE + LIFECYCLE:
            matches = [call for call in rust_calls if name in call]
            self.assertEqual(len(matches), 1, name)
            self.assertEqual(matches[0], ["env", "LOUISELM_REQUIRE_" + gate + "=1", *prefix,
                             "/bin/bash", "-c", 'umask "$1"; shift; exec "$@"', "bash", mask,
                             str(self.root / "library"), name, selection, "--nocapture"])
        python_calls = [call for call in calls if "python3" in call]
        self.assertEqual(python_calls, [["env", "LOUISELM_REQUIRE_BROKER_SYSTEMD=1", "timeout", "120",
                                       "python3", str(ROOT / "scripts/test-broker-systemd.py")]])

    def test_unknown_group_or_missing_disposable_acknowledgement_never_runs_sudo(self):
        for args in [(), ("core",), ("--disposable-guest", "typo"), ("--disposable-guest", "core", "extra")]:
            result = self.run_group(*args)
            self.assertEqual(result.returncode, 64, result.stderr)
        self.assertEqual(self.recorded(), [])

    def test_zero_selected_tests_fail_before_privileged_execution(self):
        result = self.run_group("--disposable-guest", "core", EMPTY_TEST_LIST="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no tests selected", result.stderr)
        self.assertEqual(self.recorded(), [])

    def test_fixture_failure_is_not_hidden_and_stops_the_group(self):
        result = self.run_group("--disposable-guest", "lifecycle", FAIL_SCENARIO=LIFECYCLE[0][2])
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(len(self.recorded()), 1)


class RequiredStatus(unittest.TestCase):
    def test_branch_required_status_aggregates_both_groups_and_refuses_non_success(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        def job(name):
            match = re.search(r"^  " + name + r":\n(.*?)(?=^  [a-z0-9_-]+:|\Z)", workflow, re.M | re.S)
            self.assertIsNotNone(match, name)
            return match[1]

        aggregate = job("skills-core")
        self.assertIn("    name: cargo (skills-core)", aggregate.splitlines())
        self.assertIn("    needs: skills-core-checks\n", aggregate)
        self.assertIn("    if: always()\n", aggregate)
        self.assertNotIn("continue-on-error", aggregate)
        matrix = job("skills-core-checks")
        self.assertIn("      fail-fast: false\n", matrix)
        self.assertIn("          - group: core\n", matrix)
        self.assertIn("          - group: lifecycle\n", matrix)
        self.assertNotIn("            name: cargo (skills-core)", matrix.splitlines())
        self.assertNotIn("continue-on-error", matrix)
        command = re.search(r"^      - run: (.+)$", aggregate, re.M)[1]
        expression = "${{ needs.skills-core-checks.result }}"
        self.assertIn(expression, command)
        for result in ["success", "failure", "cancelled", "skipped", ""]:
            probe = subprocess.run(["bash", "-c", command.replace(expression, result)])
            self.assertEqual(probe.returncode == 0, result == "success", result)


if __name__ == "__main__":
    unittest.main()
