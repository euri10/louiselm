#!/usr/bin/env python3
"""Private-VM gate for the real linked CLI, durable broker and read-only UID boundary."""

import json
import os
from pathlib import Path
import pwd
import runpy
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
COMMON = runpy.run_path(str(Path(__file__).with_name("test-broker-attention.py")))
command = COMMON["command"]


@unittest.skipUnless(os.environ.get("LOUISELM_REQUIRE_BROKER_ADMISSION") == "1",
                     "requires explicit disposable VM privileged gate")
class LinkedAdmission(unittest.TestCase):
    def setUp(self):
        self.phase = self.enterContext(COMMON["diagnostics"](component="broker-admission"))

    def test_cli_outage_restart_and_read_only_evidence(self):
        self.phase("validate disposable environment")
        self.assertEqual(os.geteuid(), 0)
        self.assertNotEqual(os.readlink("/proc/self/ns/mnt"), os.readlink("/proc/1/ns/mnt"))
        self.phase("locate test binaries")
        builder = runpy.run_path(str(REPO / "scripts/test-skill-requests"))
        broker_binary = os.environ.get("LOUISELM_TEST_ADMISSION_BROKER") or builder["test_binary"]("skills-core", "broker")
        skills_binary = Path(os.environ["LOUISELM_TEST_SKILLS"])
        self.phase("create scratch directory")
        with tempfile.TemporaryDirectory(prefix="admission-gate-", dir="/var/tmp") as temporary:
            scratch = Path(temporary)
            self.phase("mount private /etc")
            COMMON["mount_private_etc"](scratch)
            for target in ("/var/lib", "/run", "/usr/local/lib"):
                self.phase(f"mount private {target}")
                command("mount", "-t", "tmpfs", "-o", "mode=0755", "tmpfs", target)
            root = Path("/var/lib/louiselm-admission-gate")
            root.mkdir(mode=0o755)
            for original, name in ((broker_binary, "broker-test"), (skills_binary, "skills")):
                self.phase(f"copy {name} executable")
                shutil.copyfile(original, root / name)
                (root / name).chmod(0o555)
            source_config = Path("/etc/louiselm-broker-admission.json")
            source_config.unlink(missing_ok=True)
            accounts = []
            server = None
            try:
                for name in ("louiselm-admission-operator", "louiselm-admission-reader"):
                    self.phase("create fixture account")
                    command("useradd", "--system", "--user-group", "--no-create-home", "--shell", "/usr/sbin/nologin", name)
                    accounts.append(pwd.getpwnam(name))
                operator, broker = accounts
                private = root / "private"
                private.mkdir(mode=0o700)
                os.chown(private, operator.pw_uid, operator.pw_gid)
                state = root / "broker"
                state.mkdir(mode=0o700)
                os.chown(state, broker.pw_uid, broker.pw_gid)
                store = root / "store"
                store.mkdir(mode=0o700)
                os.chown(store, operator.pw_uid, operator.pw_gid)
                runtime = Path("/run/louiselm-operator")
                runtime.mkdir(mode=0o755)
                os.chown(runtime, broker.pw_uid, broker.pw_gid)

                def as_user(account, *args):
                    return command("setpriv", "--reuid", str(account.pw_uid), "--regid", str(account.pw_gid),
                                   "--clear-groups", *map(str, args), env={"PATH": "/usr/bin:/bin", "HOME": str(private)})

                def cli(*args):
                    return json.loads(as_user(operator, root / "skills", *args, "--store", store, "--robot-json").stdout)

                self.phase("generate fixture signing keys")
                for name in ("primary", "release", "publisher"):
                    as_user(operator, "ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", private / name)
                self.phase("bootstrap fixture trust")
                cli("trust", "bootstrap", "--primary", private / "primary.pub", "--release", private / "release.pub")
                candidate = private / "candidate"
                candidate.mkdir(mode=0o755)
                os.chown(candidate, operator.pw_uid, operator.pw_gid)
                (candidate / "SKILL.md").write_text("---\nname: skill\ndescription: Test skill.\n---\nBody.\n")
                self.phase("package fixture skill")
                package = cli("package", candidate)["digest"]
                self.phase("admit standalone skill")
                standalone = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary")
                self.assertNotIn("approval_operation", standalone["payload"])
                self.assertFalse(source_config.exists(), "standalone must not configure linkage")
                self.phase("reject unconfigured linkage")
                with self.assertRaisesRegex(RuntimeError, "not configured"):
                    cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary",
                        "--skill-request", "12345678-1234-4234-8234-123456789abc")
                provenance = store / "provenance.json"
                value = json.loads(provenance.read_bytes())
                value.update(trusted=True, created_by_release="software-key-fixture")
                provenance.write_text(json.dumps(value))  # Test-only provenance, not a production promotion.
                authority = Path("/usr/local/lib/louiselm/launcher/public-config.json")
                authority.parent.mkdir(parents=True, mode=0o755)
                authority.write_text(json.dumps({"operator_uid": operator.pw_uid, "broker_uid": broker.pw_uid, "broker_gid": broker.pw_gid}))
                authority.chmod(0o444)
                installer = REPO / "scripts/install-broker-admission.py"
                self.phase("provision read-only admission source")
                command(sys.executable, str(installer), "--store", str(store))
                original = source_config.read_bytes()
                self.phase("verify idempotent provisioning")
                command(sys.executable, str(installer), "--store", str(store))
                self.assertEqual(source_config.read_bytes(), original)
                self.phase("verify read-only skill evidence")
                as_user(broker, "test", "-r", store / "trust/roles.json")
                as_user(broker, "test", "!", "-w", store / "trust/roles.json")
                as_user(broker, "test", "!", "-r", private / "primary")
                (candidate / "SKILL.md").write_text("---\nname: skill\ndescription: Test skill.\n---\nChanged body.\n")
                subsequent = cli("package", candidate)["digest"].replace(":", "-", 1)
                as_user(broker, "test", "-r", store / "packages" / subsequent / "files/SKILL.md")
                as_user(broker, "test", "!", "-w", store / "packages" / subsequent)

                def start(calls, packages=None, supply=False, source_packages=None, cancel_source=False):
                    env = {"PATH": "/usr/bin:/bin", "HOME": str(state), "LOUISELM_ADMISSION_STATE": str(state),
                           "LOUISELM_ADMISSION_OPERATOR": str(operator.pw_uid), "LOUISELM_ADMISSION_PACKAGE": package,
                           "LOUISELM_ADMISSION_CALLS": str(calls)}
                    if packages is not None:
                        env["LOUISELM_ADMISSION_PACKAGES"] = json.dumps(packages)
                    if supply:
                        env["LOUISELM_ADMISSION_SUPPLY"] = "1"
                    if source_packages is not None:
                        env["LOUISELM_ADMISSION_SOURCES"] = json.dumps(source_packages)
                    if cancel_source:
                        env["LOUISELM_ADMISSION_CANCEL_SOURCE"] = "1"
                    child = subprocess.Popen(["setpriv", "--reuid", str(broker.pw_uid), "--regid", str(broker.pw_gid),
                                              "--clear-groups", str(root / "broker-test"), "--ignored", "--exact",
                                              "skill_requests::installed_linked_admission_server", "--nocapture"], env=env)
                    deadline = time.monotonic() + 10
                    while not (runtime / "inspect.sock").exists():
                        if child.poll() is not None or time.monotonic() > deadline:
                            child.terminate()
                            child.wait(timeout=5)
                            self.fail("broker readiness failed")
                        time.sleep(0.02)
                    return child

                self.phase("start broker before signing")
                before = set((store / "generations").glob("*.json"))
                server = start(1)  # Lose broker after inspection, before reservation.
                operation = (state / "operation").read_text()
                with self.assertRaisesRegex(RuntimeError, "broker did not reserve signing"):
                    cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary", "--skill-request", operation)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(set((store / "generations").glob("*.json")), before)
                server = start(2)  # Reserve, then lose delivery after signing.
                self.phase("admit with broker loss")
                linked = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary", "--skill-request", operation)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(linked["admission"]["state"], "pending_witness")
                self.assertIsNone(linked["broker_status"])
                self.assertEqual(linked["resolution_error"], "broker_unavailable")
                self.assertEqual(json.loads((state / "status").read_bytes())["outcome"], "pending")
                self.phase("restart broker for durable evidence")
                server = start(3)  # Startup reconciles actual durable evidence; no new signature.
                self.phase("retry durable admission")
                retry = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "missing-key", "--skill-request", operation)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(retry["admission"], linked["admission"])
                self.assertEqual(retry["broker_status"]["outcome"], "approved")
                server = start(1)
                replay_without_broker = cli("generation", "admit", "--member", package + ":read=codex",
                                            "--key", private / "missing-key", "--skill-request", operation)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(replay_without_broker["admission"], linked["admission"])
                self.assertIsNone(replay_without_broker["broker_status"])
                self.assertEqual(replay_without_broker["resolution_error"], "broker_unavailable")
                as_user(broker, "test", "-r", store / "trust/roles.json")
                as_user(broker, "test", "!", "-w", store / "generations")
                self.assertFalse((store / "pins.jsonl").exists(), "approval never activates supply")

                self.phase("prepare batched profile request")
                profile = private / "profile"
                profile.mkdir(mode=0o755)
                os.chown(profile, operator.pw_uid, operator.pw_gid)
                (profile / "PROFILE.md").write_text(
                    "---\ntype: louiselm/operator-profile\n---\nPrivate preference.\nSecond change.\n")
                profile_package = cli("package", profile)["digest"]
                reviewed_profile = cli("dossier", profile_package, "--review-depth", "read")["review_digest"]
                packages = sorted([package, profile_package])
                state = root / "profile-state"
                state.mkdir(mode=0o700)
                os.chown(state, broker.pw_uid, broker.pw_gid)
                registry = root / "registry"
                registry.mkdir(mode=0o755)
                registry_file = registry / "agents.json"

                def registry_agents(names):
                    entries = [{"id": name, "provider": "fixture", "runtime_id": "fixture",
                                "arguments": [], "environment": {}} for name in names]
                    registry_file.write_text(json.dumps({"schema": "louiselm.launch.registry/1", "entries": entries}))
                    registry_file.chmod(0o644)

                members = [item for digest in packages for item in ("--member", digest + ":read=codex")]
                server = start(1, packages, True)
                operation = (state / "operation").read_text()
                with self.assertRaisesRegex(RuntimeError, "needs the configured Agent list"):
                    cli("generation", "admit", *members, "--key", private / "primary", "--skill-request", operation,
                        "--reviewed-profile-dossier", reviewed_profile)
                self.assertEqual(server.wait(timeout=10), 0)
                registry_agents(["codex", "opencode"])
                server = start(1, packages, True)
                with self.assertRaisesRegex(RuntimeError, "does not reach configured Agents: opencode"):
                    cli("generation", "admit", *members, "--key", private / "primary",
                        "--skill-request", operation, "--registry", registry,
                        "--reviewed-profile-dossier", reviewed_profile)
                self.assertEqual(server.wait(timeout=10), 0)
                registry_agents(["codex"])
                server = start(1, packages, True)
                with self.assertRaisesRegex(RuntimeError, "requires the reviewed profile Dossier digest"):
                    cli("generation", "admit", *members, "--key", private / "primary",
                        "--skill-request", operation, "--registry", registry)
                self.assertEqual(server.wait(timeout=10), 0)
                server = start(2, packages, True)
                linked_profile = cli("generation", "admit", *members, "--key", private / "primary",
                                     "--skill-request", operation, "--registry", registry,
                                     "--reviewed-profile-dossier", reviewed_profile)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(len(linked_profile["admission"]["payload"]["members"]), 2)
                self.assertEqual(linked_profile["admission"]["state"], "pending_witness")
                server = start(3, packages, True)
                replay = cli("generation", "admit", *members, "--key", private / "missing-key",
                             "--skill-request", operation, "--registry", registry)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(replay["broker_status"]["outcome"], "approved")
                self.assertEqual(replay["admission"], linked_profile["admission"])
                self.assertFalse((store / "pins.jsonl").exists())

                self.phase("prepare independently submitted profile sources")
                source_packages = []
                for index in range(2):
                    source_candidate = private / f"source-profile-{index}"
                    source_candidate.mkdir(mode=0o755)
                    os.chown(source_candidate, operator.pw_uid, operator.pw_gid)
                    (source_candidate / "PROFILE.md").write_text(
                        f"---\ntype: louiselm/operator-profile\n---\nSource preference {index}.\n")
                    source_packages.append(cli("package", source_candidate)["digest"])

                state = root / "cancelled-source-state"
                state.mkdir(mode=0o700)
                os.chown(state, broker.pw_uid, broker.pw_gid)
                server = start(2, packages, True, source_packages, True)
                operation = (state / "operation").read_text()
                sources = json.loads((state / "sources").read_bytes())
                source_args = [item for source_id in sources for item in ("--source-request", source_id)]
                before = set((store / "generations").glob("*.json"))
                with self.assertRaisesRegex(RuntimeError, "broker did not reserve signing"):
                    cli("generation", "admit", *members, "--key", private / "primary",
                        "--skill-request", operation, "--registry", registry,
                        "--reviewed-profile-dossier", reviewed_profile, *source_args)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(set((store / "generations").glob("*.json")), before)
                self.assertIn("cancelled", [item["outcome"] for item in json.loads((state / "source-statuses").read_bytes())])

                state = root / "signed-sources-state"
                state.mkdir(mode=0o700)
                os.chown(state, broker.pw_uid, broker.pw_gid)
                server = start(2, packages, True, source_packages)
                operation = (state / "operation").read_text()
                sources = json.loads((state / "sources").read_bytes())
                source_args = [item for source_id in sources for item in ("--source-request", source_id)]
                linked_sources = cli("generation", "admit", *members, "--key", private / "primary",
                                     "--skill-request", operation, "--registry", registry,
                                     "--reviewed-profile-dossier", reviewed_profile, *source_args)
                self.assertEqual(server.wait(timeout=10), 0)
                server = start(3, packages, True, source_packages)
                replay = cli("generation", "admit", *members, "--key", private / "missing-key",
                             "--skill-request", operation, "--registry", registry,
                             "--reviewed-profile-dossier", reviewed_profile, *source_args)
                self.assertEqual(server.wait(timeout=10), 0)
                self.assertEqual(replay["admission"], linked_sources["admission"])
                self.assertEqual(replay["broker_status"]["outcome"], "approved")
                statuses = json.loads((state / "source-statuses").read_bytes())
                self.assertEqual([item["outcome"] for item in statuses], ["superseded", "superseded"])
                self.assertTrue(all(item["admission"] == linked_sources["admission"]["generation"] for item in statuses))
                before = set((store / "generations").glob("*.json"))
                for changed_sources in ([], sources[:1]):
                    server = start(2, packages, True, source_packages)
                    changed_args = [item for source_id in changed_sources for item in ("--source-request", source_id)]
                    with self.assertRaisesRegex(RuntimeError, "Admission already signed; source requests could not be confirmed"):
                        cli("generation", "admit", *members, "--key", private / "missing-key",
                            "--skill-request", operation, "--registry", registry,
                            "--reviewed-profile-dossier", reviewed_profile, *changed_args)
                    self.assertEqual(server.wait(timeout=10), 0)
                    self.assertEqual(set((store / "generations").glob("*.json")), before)

                self.phase("prepare separately activated withdrawal supply")
                store = root / "withdrawal-store"
                store.mkdir(mode=0o700)
                os.chown(store, operator.pw_uid, operator.pw_gid)
                cli("trust", "bootstrap", "--primary", private / "primary.pub", "--release", private / "release.pub")
                cli("endorser", "add", "--endorser", private / "publisher.pub", "--label", "Fixture publisher",
                    "--role", "publisher", "--allow-software-key", "--key", private / "primary")
                package = cli("package", candidate)["digest"]
                positive = private / "positive"
                cli("endorsement", "sign-batch", "--member", package, "--role", "publisher",
                    "--endorser", private / "publisher.pub", "--key", private / "publisher", "--output", positive)
                cli("endorsement", "import", positive)
                admitted = cli("generation", "admit", "--member", package + ":read=codex", "--key", private / "primary")
                self.assertEqual(len(admitted["payload"]["members"][0]["relied_on"]), 1)
                remote = private / "witness.git"
                as_user(operator, "git", "init", "--bare", "--quiet", remote)
                cli("generation", "witness", admitted["generation"], "--remote", remote)
                cli("generation", "activate", admitted["generation"])
                pins = (store / "pins.jsonl").read_bytes()
                provenance = store / "provenance.json"
                value = json.loads(provenance.read_bytes())
                value.update(trusted=True, created_by_release="software-key-fixture")
                provenance.write_text(json.dumps(value))  # Only this disposable fixture is promoted.
                source_config.unlink()  # Isolated fixture switches to the separately activated store.
                self.phase("provision withdrawal evidence for distinct reader UID")
                command(sys.executable, str(installer), "--store", str(store))
                for evidence in [store / "endorsers/local.json", *sorted((store / "endorsements").glob("*.json"))]:
                    as_user(broker, "test", "-r", evidence)
                    as_user(broker, "test", "!", "-w", evidence)
                as_user(broker, "test", "!", "-r", store / "endorsers/local.lock")
                as_user(broker, "test", "!", "-r", private / "publisher")

                self.phase("import withdrawal after provisioning")
                withdrawals = private / "withdrawals"
                cli("endorsement", "withdraw", "--member", package, "--role", "publisher",
                    "--endorser", private / "publisher.pub", "--key", private / "publisher", "--output", withdrawals)
                cli("endorsement", "import", withdrawals)
                for evidence in (store / "endorsements").glob("*.json"):
                    as_user(broker, "test", "-r", evidence)
                    as_user(broker, "test", "!", "-w", evidence)
                withdrawal_state = root / "withdrawal-state"
                withdrawal_state.mkdir(mode=0o700)
                os.chown(withdrawal_state, broker.pw_uid, broker.pw_gid)

                def reconcile_withdrawal(expected):
                    as_user(broker, "env", f"LOUISELM_ADMISSION_STATE={withdrawal_state}",
                            f"LOUISELM_ADMISSION_WITHDRAWAL_EXPECT={expected}", root / "broker-test",
                            "--ignored", "--exact", "supply_withdrawals::installed_supply_withdrawal_source", "--nocapture")

                self.phase("raise withdrawal Attention without a Session")
                reconcile_withdrawal("raise")
                self.phase("clear withdrawal Attention after operator quarantine")
                cli("quarantine", "exclude", package, "--reason", "fixture withdrawal")
                as_user(broker, "test", "-r", store / "quarantine.json")
                as_user(broker, "test", "!", "-w", store / "quarantine.json")
                reconcile_withdrawal("clear")
                self.assertEqual((store / "pins.jsonl").read_bytes(), pins, "read-only projection never activates supply")
            finally:
                self.phase("cleanup broker")
                if server is not None and server.poll() is None:
                    server.terminate()
                    server.wait(timeout=5)
                for account in reversed(accounts):
                    self.phase("delete fixture account")
                    command("userdel", account.pw_name)
                self.phase("unmount private /etc")
                command("umount", "/etc")
                self.phase("remove scratch directory")
        self.phase("complete")


if __name__ == "__main__":
    unittest.main()
