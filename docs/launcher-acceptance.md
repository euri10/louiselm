# Verified launcher acceptance audit

Recorded 2026-09-06 for `louiselm-d6fv.4.7`, against `3074423` plus the
`louiselm-vi55` status-read fix. Session:
`codex/01a074f5-e592-72d1-9ccf-69e162d8cdfe`.

Updated the same day for `louiselm-ln30`: trusted registry opening now checks
every runtime descendant, not only the executable and listed adapters.
`louiselm-own-quiesce-relay-workers-mthx` adds joined, cancellable production
relay ownership. The broader composition still fails on unchanged `43e5164`;
the accepted `louiselm-6f7q` creator-thread fix is not an ancestor of this checkout.

**The launcher parent is not accepted.** Implementation coverage, host-mechanism
checks, installed authority, and end-to-end Verified posture are separate claims.
The [disposable VM](launcher-vm.md) is the only privileged test environment used
here; no desktop sudo, security changes, credentials, or host mounts were used.

## Acceptance map

AC numbers refer to `br show louiselm-d6fv.4 --json`. Paths and named tests below
are evidence pointers, not a claim that every production composition was run.

| AC | Existing boundary and evidence | Remaining acceptance |
| --- | --- | --- |
| 1 — installed signed launcher | [Binary `run`](../skills-core/src/bin/louiselm-launch.rs) requires root, verified running release, installed release match and exact operator UID. [Release tests](../skills-core/tests/release.rs) cover development identity, signature/tamper and ownership refusal. | Genuine hardware-signed release ceremony `louiselm-lm70`; installed launcher/sudo ceremony `louiselm-d6fv.4.9`. The destructive UID probe found here is fixed by `louiselm-vi55`. |
| 2 — closed identifier request | [`LaunchRequest`, `resolve`](../skills-core/src/launch.rs) and [request tests](../skills-core/tests/launch.rs) reject extra fields, noncanonical/oversized input, malformed IDs and runtime path traversal. Runtime is resolved through the registered Agent, not a caller command. | Generation/input IDs are shape-checked and bound, not loaded or independently verified here. Real authorization is `louiselm-qbr.5.1.1`; materialized inputs are `louiselm-d6fv.3`. |
| 3 — distinct identity/private state/channels | [Sandbox](../skills-core/src/sandbox.rs), [identity leases](../skills-core/src/launcher_install/identity.rs), and [system platform](../skills-core/src/launch_supervisor/system.rs) own UID/GID, private home/workspace, cgroup and capability socket. Guest root checks established outer identity and private-directory ownership. | Simultaneous hostile cross-Session probes in `louiselm-d6fv.4.8`; actual input/workspace/cache supply remains `louiselm-d6fv.3`/`louiselm-d6fv.5`. Current `resolve` creates paths, not a populated input snapshot. |
| 4 — no ambient authority | Bubblewrap uses mount/PID/IPC/network namespaces, a cleared environment and fixed system roots. `a_confined_session_cannot_reach_paths_outside_its_plan` denies one secret path. | Full process/socket/config/network denial matrix `louiselm-d6fv.4.8`; one invisible file is insufficient evidence for every listed escape class. |
| 5 — measured fixed runtime | [`RuntimePackage::measure`](../skills-core/src/registry.rs) hashes the executable and listed adapters, and records version/origin/library baseline/policy version. `Registry::open_trusted` now validates ownership, permissions and types throughout the entire mounted runtime tree (`louiselm-ln30`). Production validates the measured Bubblewrap path before preparation. | Library-baseline/policy strings are recorded, not independently remeasured or compared here. Unlisted files are protected by root ownership, not added to the digest format; root compromise remains outside the boundary. Provider packaging/masking belongs to `louiselm-d6fv.3`. |
| 6 — complete isolation evidence | [`IsolationEvidence::check`](../skills-core/src/isolation.rs) rejects wrong contract, missing kernel flags, missing/duplicate/unsatisfied dimensions. [Isolation tests](../skills-core/tests/isolation.rs) cover these cases; supervisor rejects bad evidence before signing. | Most backend dimension booleans describe invoked mechanisms, not observed hostile-probe outcomes. Only identity and cgroup lifecycle have stronger direct startup observations. Do not equate structurally complete evidence with completed conformance (`louiselm-d6fv.4.8`). |
| 7 — hostile conformance | Existing [sandbox tests](../skills-core/tests/sandbox.rs) cover private-path denial, outer identity, forged identity observations, startup gates and descendant cleanup. Bootstrap/transport tests cover their descriptor-transfer and credential boundaries. | Missing composite probes are enumerated below and assigned to `louiselm-d6fv.4.8`. |
| 8 — whole-tree lifecycle | [Lifecycle owner](../skills-core/src/launch_supervisor/lifecycle.rs) serializes mechanics, loss and receipts. [Supervisor tests](../skills-core/tests/launch_supervisor.rs) cover Park/Resume/interrupt/Disposal, races, reconnect and retained leases using doubles. Real cgroup tests cover freeze/thaw, interrupt and grandchildren. `mthx` adds owned relay cancellation/join and [production tests](../skills-core/src/launch_supervisor/system_relay_tests.rs) proving controller I/O closure on quiescence and direct Disposal. | Integrate the accepted `louiselm-6f7q` creator-thread fix before claiming composite acceptance on this checkout. `louiselm-relay-failure-terminal-receipt-w1ez` still omits terminal receipt on relay failure. Emergency quarantine propagation remains `louiselm-d6fv.6.5`. |
| 9 — integrity-protected launch receipt | [`run_launch`/`launch_evidence`](../skills-core/src/launch_supervisor.rs) binds request/runtime/input/isolation/kernel-prerequisite digests and channel IDs. Starting ACK gates start; linked Running ACK gates success. [Receipt tests](../skills-core/tests/launch_receipt.rs) reject mutation, gaps, duplicates, foreign prefixes and splices. | Real signer, installed entrypoint, real broker durability and live Session must be exercised together through `louiselm-qbr.5.1.1` and `louiselm-d6fv.9`. The privileged composite test uses a fake signer/broker and a test platform around real Bubblewrap, not `SystemLaunchPlatform` end to end. |
| 10 — direct CLI is unmanaged | Direct vendor launch has no launcher receipt. There is no `louiselm-launch` call in the current Lua Session launch path; development release identity stays unverified. | User-visible Verified posture activation is exclusively `louiselm-d6fv.9`, with launch-preview/health work in `louiselm-d6fv.6.2`. Do not call current Sessions verified. |

## Hostile matrix: what still needs a probe

The scope is the actual confined workload, not only schema validation or an
unconfined transport peer. Each negative test needs a positive control: an
absent socket, dead target process, or unreachable network endpoint proves
nothing about containment.

| Attack class | Current evidence | Gap |
| --- | --- | --- |
| Operator home/checkout/credentials | One outside-file read denied. | Dedicated harmless sentinels for each root, including traversal through a Session-controlled symlink. |
| `/proc`, ptrace and signals | PID namespace flags; host-side observation of assigned credentials/maps. | Workload attempts against operator, launcher, broker and a second live Session, with target survival verified. |
| Inherited descriptors/environment/terminal | Bootstrap validates declared descriptor transfers; environment is cleared and a new session requested. | Seed undeclared open file/socket descriptors and ambient values before launch; prove none reach the workload. |
| Unix sockets | Capability peer credentials, membership and revoke/reenable are tested. | Ambient pathname and abstract-socket reachability attempts from confinement. |
| D-Bus, user systemd, Docker | Host sockets are not deliberately mounted. | Controlled service/socket sentinels and a service-mediated execution attempt; no real desktop service or Docker authority. |
| Native Provider config/MCP | Private home plus registry-controlled environment. | Sentinel discovery attempts; actual Provider input masking remains `louiselm-d6fv.3`. |
| Runtime/config mutation | Listed executable mutation is rejected; backend bytes are rechecked before prepare. `louiselm-ln30` adds whole-runtime trust checks and a real UID-1000 write/denial probe for unlisted config. | Composite confined-workload writes and alternate runtime inputs still belong to `louiselm-d6fv.4.8`. |
| Cross-Session access | Private modes and leased identity primitives. | Two simultaneous Sessions attempting each other's files, processes and capability channels. |
| Network | Non-denied network policy refused; empty network namespace requested. | Actual IPv4/IPv6/loopback attempts. The outer VM's restricted network is an additional desktop guard, not proof of the inner Session policy. |
| Child survival/Park/interrupt | Real cgroup freeze/thaw, interrupt, grandchild Disposal and root-owned cgroup anti-migration checks; production relay cancellation/join with an open controller. | Combine with hostile forking and failure/loss through production lifecycle; integrate creator-thread lifetime fix and finish terminal receipt bug. |

## Evidence from the VM

Environment: Debian 13, kernel `6.12.107+deb13-cloud-amd64`, Bubblewrap `0.12.0`,
Rust `1.97.1`, root tests in the initial guest user namespace. This establishes
evidence for this environment, not every Linux host.

The earlier `louiselm-d6fv.4.6` record contains four passing Rust gates and five
explicit privileged checks (no skips in those targeted invocations):

- `host_identity_changes_outer_credentials_and_owns_private_directories`
- `pinned_root_cgroup_prevents_operator_migration_of_a_session_process`
- `privileged_supervisor_launches_agent_under_the_assigned_outer_identity`
- `production_prepare_rejects_bubblewrap_changed_after_runtime_config_before_spawn`
- `trusted_registry_requires_root_owned_immutable_documents_and_runtime`

Ordinary `cargo test` success is not a no-skip claim: privileged/cgroup tests
can return early when their prerequisites are absent. The future required
conformance recipe must fail clearly in that situation.

This audit additionally reproduced two failures through public Rust APIs:

- `louiselm-ln30`: UID 1000 rewrote `provider-config.json` inside a root-created
  runtime; `RuntimeMeasurement` stayed identical and `Registry::open_trusted`
  still accepted it before the fix. Four new regression groups failed before
  whole-tree validation and passed after it: operator mutation, untrusted
  descendant modes/owners, symlinks/special files, and unreadable enumeration.
  The existing ownership case also passes. Immutable unlisted files remain
  accepted, while UID 1000 cannot rewrite them. No VM escape or real Provider
  exploit was claimed.
- `louiselm-vi55`: `install::status` followed its predictable UID-probe symlink,
  truncated a fixture sentinel and removed the link. The committed
  `status_preserves_a_preexisting_uid_probe_and_its_target` regression failed
  before replacing the probe with `rustix::process::geteuid()`. The same-UID
  reproduction does not establish a cross-UID exploit under every host's
  `protected_symlinks` policy.

The subsequent `mthx` pass reproduced successful quiescence leaving the
controller socket owned (`WouldBlock` instead of EOF), then passed both real
Bubblewrap cleanup cases in five consecutive guest-root rounds. Ordinary tests
also cover opaque partial I/O, blocked output, callback backpressure, cancellation
before attachment, spawn failure and worker panic. `louiselm-m9ov` records the
red/green descriptor-alias flag-restoration regression found during this work.

This is component evidence, not a newly green composite launch: the privileged
ACP-echo test fails in 5.04s on unchanged `43e5164`, as well as with `mthx`.
The closed `louiselm-6f7q` record names fixes `7ccb05a`/`427c0ba` on
`fix/6f7q-launch-thread` (PR #3), which this checkout does not contain. Its
integration and renewed composite acceptance are tracked by `louiselm-1mac`,
an explicit launcher blocker. No branch was
merged or privileged desktop setting changed during this pass.

Issue comments carry command results and fix verification. Temporary diagnostic
source is retained at
`/home/lotso/.cache/louiselm-launcher-vm/audit_registry.rs`; reproduction steps
and source locations also live in the bugs, so the cache is not the sole record.

## Completion and dependency boundaries

- `louiselm-d6fv.4.7` is the coverage audit, not the conformance implementation.
  `louiselm-d6fv.4.8` owns the hostile suite; `louiselm-d6fv.4.9` owns installed
  launcher authority and waits on `louiselm-lm70` for genuine release acceptance.
- `louiselm-ln30` and relay worker ownership (`mthx`) are fixed. Relay-failure
  terminal receipts and integration of the creator-thread fix still block
  launcher acceptance. The duplicate lifecycle-history allocation issue
  `louiselm-bound-lifecycle-replay-history-wg2v` remains separately tracked;
  this audit does not claim a bounded-memory production lifecycle.
- Do not add reverse dependencies on `louiselm-qbr.5.1` or `louiselm-d6fv.9`:
  those aggregates already depend on the launcher. They own integrated broker
  and Verified activation acceptance. Their absence must remain explicit in
  any component-level handoff.
- Children `.4.1`, `.4.2`, `.4.3` closed with `inert:` verdicts; `.4.4` closed
  with `gate:` and `.4.5` with `consumer:`. Later code wiring does not silently
  rewrite those historical verdicts. Under the project contract the parent
  cannot receive a stronger verdict than its weakest child.
- No fake signing key, test broker, successful backend test, or mechanism name
  substitutes for the genuine signed-release/installed workflow. Hardware
  ceremonies require maintainer coordination; do not add token passthrough or
  host privileges to this VM implicitly.
