# Broker launch authority gate

`louiselm-qbr.5.1.1.5.4` composes the real `InstalledBroker`,
`InstalledLaunchSigner`, `SystemLaunchPlatform` and `LaunchSupervisor` with the
release-measured native Agent/helper. The broker runs as its own unprivileged
account; the per-Session supervisor retains privileged mechanics. This is a
disposable installed-configuration fixture, not the desktop installation or a
vendor Agent integration. Recovery remains `louiselm-qbr.5.1.2`, and fully
Verified cutover remains `louiselm-d6fv.9`.

The ordered contract is exact sequence-0 Launch/Starting durability, restricted
initialization, actual Agent identity and tool-isolation verification, capability
enablement, then exact sequence-1 Start/Running durability before launch success.
Receipt schema `/4` binds the supervisor-established Agent PID, assigned UID/GID
and isolation-evidence digest. The broker binds the original persisted command
approval to that evidence before sending the final ACK. Initial inspection
reads one receipt-chain snapshot for the durable head and signed prerequisites.
`inspect` reports `LaunchObservation::DurableOnly`; `inspect_active` additionally
reports the owning worker's completed ACK send and locally open/closed channel.
Neither proves present Agent liveness. In particular, a sequence-1 receipt can
be durable while the final audit or ACK fails: its recorded state is Running,
but no acknowledged `BrokerSession` is returned. Never label a live Session from
the stored state alone.

| Boundary | Check |
| --- | --- |
| Dedicated installed identities and real signatures | `privileged_installed_broker_launch_and_effects`: root broker refusal, unprivileged broker denied private config/key access, signed launch, actual command, denial, explicit helper and terminal cleanup/identity reuse. |
| Signer and durable storage | `privileged_installed_launch_failures_never_acknowledge_success`: both sequence-0 and sequence-1 failures, plus a cryptographically valid signature over different bytes. No launch success, effect or retained usable identity. |
| Missing or contradictory proof | `start_receipt_requires_agent_authority_evidence`, `a_signed_start_cannot_change_the_installed_identity`, `known_unsupported_integration_refuses_before_authorization_or_spawn`, and measured integration checks. |
| Approval and ordinary absence | `pending_approval_is_exact_durable_and_expiring`, `absent_command_approval_denies_effects_without_ending_the_session`, command/grant authority tests. |
| Connector identity and initial inspection | `broker::launch_gates` tests reject foreign first connectors and inherited supervisor sockets before consumption; `durable_running_without_final_audit_never_claims_an_acknowledged_channel` distinguishes receipt evidence from a completed handshake. |
| Isolation, revocation and uncertain outcomes | Existing measured Agent/helper gate plus command dispatch, grant dispatch and supervisor lifecycle suites retain wrong sender/child, queued/running revocation, local expiry, Agent exit, stale callbacks, failed cleanup/poison, lost replies and late actual outcomes. |

CI requires both installed composition tests alongside the existing measured
Agent/helper gates. To repeat, build all binaries and the library test, transfer
only the selected artifacts or sources, and follow [the VM boundary](launcher-vm.md).
Inside the disposable guest, run the library test as root with
`LOUISELM_REQUIRE_BROKER_LAUNCH=1`, selecting each exact test name under
`launch_supervisor::system::installed_tests`. Run them sequentially: the fixture
owns one dedicated temporary account and fixed Session cgroup. It creates a fresh
software receipt key, invokes the real installer with temporary authority paths,
and removes its account and private fixture state at teardown. No hardware key,
guest egress, package installation or privileged host change is needed.

The fixture reuses the production supervisor and installed public configuration
APIs while choosing a temporary root-owned registry and Session directory. The
fixed sudo entrypoint and release-signature installation boundary have their
separate [installed authority acceptance](launcher-authority-acceptance.md).
Neither fixture result alone establishes the complete desktop/vendor workflow.

## Recorded acceptance — 2026-09-11

The final `skills-core` suite passed **675 tests**, with three existing ignored
tests, via `scripts/test-skills-core` in a transient user service outside ACP
(invocation `9768bc3d15d94509a178609a15f5a3a4`). Formatting, all-target/all-feature
Clippy with warnings denied, Rustdoc with warnings denied, the 13 Node browser
tests, `git diff --check`, and `scripts/check-agent-instructions` passed.
An intermediate full run reobserved the existing admission lock-release flake
`louiselm-h157` at `tests/admission.rs:442`; its isolated and final full reruns
passed. That unrelated defect remains open; no admission code was changed.

The normal, restricted-network launcher VM ran the installed positive test,
all five installed faults (`signer0`, `signer1`, `storage0`, `storage1`,
`signature0`), the measured Agent isolation gate, and all nine helper scenarios
(`complete`, `queued_revoke`, `running_revoke`, `expiry`, `agent_exit`,
`lost_grant`, `transferred_descriptor`, `failed_cleanup`, `late_actual`).
These privileged cases were explicitly enabled, not their ordinary-suite skips.

The guest lacks OpenSSL development headers, so no guest compilation is claimed:
only the selected host-built library test and three measured binaries were
transferred. No package installation, guest egress, token/hardware signing, host
installation or vendor acceptance was attempted. The fixture uses a temporary
software key and temporary installed authority paths; release-signature admission
and the fixed sudo entrypoint remain the separate acceptance cited above.
