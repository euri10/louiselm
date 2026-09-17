# Broker launch authority gate

Continuing lifecycle authorization and Attention projection are documented in
[Broker lifecycle and Attention delivery](broker-lifecycle.md).

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

## Aggregate acceptance map

Criteria below refer to the launch aggregate `louiselm-qbr.5.1.1` and its
integration child `louiselm-qbr.5.1.1.5`. Test names are searchable in
`skills-core/tests` and `skills-core/src/launch_supervisor`.

| Criteria | Boundary and check |
| --- | --- |
| Launch 1–2; integration 1 | Persisted exact approval and atomic consumption: `pending_approval_is_exact_durable_and_expiring`, `one_authorization_is_consumed_once_and_stays_consumed_across_restart`, `concurrent_consumption_has_exactly_one_winner`, expiry/controller/request mismatch and identity-pool exhaustion tests in `tests/broker.rs`. `a_signed_start_cannot_change_the_installed_identity` rejects substituted installed identity. |
| Launch 3–4; integration 1 | Authenticated exact receipt ownership: `broker::launch_gates` rejects foreign first connectors and inherited supervisor sockets before consumption. `both_launch_receipts_are_durably_stored_with_their_exact_bytes`, signature/predecessor/authorization rejection and restart-chain tests cover append validation. `privileged_installed_broker_launch_and_effects` proves dedicated unprivileged broker identity, denied private config/key access and real signatures. |
| Launch 4–5; integration 2–3 | Ordered durability and failures: `launch_acks_starting_then_starts_and_acks_linked_running_before_success` and `capability_binding_waits_for_restricted_agent_startup`. `privileged_installed_launch_failures_never_acknowledge_success` injects both sequence-0 and sequence-1 signer/storage failures, plus a valid signature over different bytes. No launch success, effect or retained usable identity. |
| Launch 5–6; integration 1–3 | Required authority proof: `start_receipt_requires_agent_authority_evidence`, `known_unsupported_integration_refuses_before_authorization_or_spawn`, `missing_tool_isolation_disposes_restricted_startup_without_enabling_effects`, `reaper_foreign_process_and_mismatched_agent_ids_never_bind_authority`, and cleanup-proof/poison tests. |
| Launch 6; integration 4 | Original approval and ordinary absence: `absent_command_approval_denies_effects_without_ending_the_session` plus command/grant authority tests. The installed positive gate exercises the intended Agent, ungranted command denial, explicitly constrained helper and terminal cleanup/identity reuse. Agent-proxied requests remain Agent actions under its original approval, not prompt-injection immunity. |
| Launch 6; integration 4 | Lifetime-pinned isolation and revocation: measured Agent/helper gates plus command dispatch, grant dispatch and supervisor lifecycle suites retain per-message sender/child checks, queued/running revocation, local expiry, Agent exit, stale callbacks, failed cleanup/poison, lost replies and late actual outcomes. |
| Launch 7; integration 3–4 | Truthful bounded inspection/audit: `the_operator_record_stays_normalized_after_a_launch` and `a_refused_launch_is_recorded_as_a_stable_error`. `durable_running_without_final_audit_never_claims_an_acknowledged_channel` distinguishes signed durable state from completed channel handoff; neither establishes current Agent liveness or fully Verified posture. |
| Integration 5 | This document, required CI tests and the separate installed-authority acceptance retain the production boundary. Later recovery stays in `louiselm-qbr.5.1.2`; desktop/vendor cutover and the fully Verified claim stay in `louiselm-d6fv.9`. |

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

## Provider credential custody

`launch_supervisor::system::installed_tests::provider_credentials::privileged_installed_provider_credentials_stay_broker_side`
extends the installed launch fixture with a random synthetic credential. CI runs
it explicitly with `LOUISELM_REQUIRE_BROKER_LAUNCH=1`; the ordinary suite skips
the privileged body. It first starts the actual installed broker with no
configured Providers, then checks rejection of unsafe credential ownership and
permissions, and finally launches a real confined Agent with valid broker-held
material. The credential never becomes a launch input.

The fixture checks exact broker UID/GID and directory mode, handle-only output,
denied reads from the assigned Session UID, process environments and arguments,
Session/registry files, and durable authorization, receipt and audit records.
Unit tests additionally cover symlinks, hard links, FIFOs, special mode bits,
missing/unreadable files and malformed or oversized contents. Receipt and audit
tests reject credential fields through the existing closed schemas. No real
Provider credential or network request is used. Provider request mediation and
the Verified-launch gate remain the sibling tasks named in the crate README.

Recorded 2026-09-17: the explicitly enabled custody gate passed in the restricted
launcher VM, using the host-built library test and four selected binaries.
The guest disk was full, so the artifacts ran from guest tmpfs with private
mounts for the fixture's `/etc` and `/var/lib`; no prior guest files were removed.
The complete `skills-core` suite passed through the documented transient-service
wrapper, along with formatting, Clippy, Rustdoc and all 13 browser tests. The
maintainer host has no installed broker authority; this records disposable
installed-composition acceptance, not desktop deployment or Provider networking.

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

The aggregate audit reused those explicitly enabled VM results from `c44f3bb`:
the Rust implementation, tests and CI definition remain unchanged. A fresh full
suite passed 675 tests with three existing ignored tests (invocation
`442bb406e1a049a39367a42fcb5ef27c`); formatting, Clippy, Rustdoc, all 13 browser
tests and the instruction-size check passed again. This close-out changes only
acceptance documentation and tracker evidence, so no new red-green test is
warranted. Both aggregates retain the earlier children's weakest close verdict,
`inert:louiselm-d6fv.9`; completed initial-launch integration does not establish
desktop/vendor reachability or authorize a fully Verified claim.
