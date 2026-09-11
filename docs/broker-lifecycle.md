# Broker lifecycle and Attention delivery

`louiselm-qbr.5.1.2` owns the full recovery/status integration. This document
describes its implemented authorization and projection boundaries. Restart
reattachment, controller-loss settlement, the operator CLI and Agent self-status
remain open work. The confirmed design in `louiselm-c0vs` requires broker-owned
recovery evidence backed by trusted supervisor retention proof; a reference
string alone must not authorize disposal. `louiselm-qbr.5.1.2.5` supplies the
retention mechanics below. Authenticated controller registration/admission and
broker protocol wiring remain open in `louiselm-qbr.5.1.2.6`.

## Recovery retention mechanics

`RunningAgent::retain_recovery` takes a path-free operation ID, ACP identity and
absolute Park expiry. The installed adapter requires the measured deterministic
Agent and an idle frozen tree, stops owned tools/helpers, and performs bounded
storage I/O on an owned worker. Resume, tool execution and disposal join that
worker; disposal seals the pinned original Session directory before reporting
cleanup success to the identity-lease owner. Failed sealing cannot authorize
identity reuse.

The supported layout is **only** `louiselm.test-recovery/1`, the synthetic
counter protocol in `louiselm-tool-test-agent`. Its closed
`home/recovery.json` checkpoint and `workspace/recovery-counter.json` must agree.
Unknown layouts, missing records, extra fields, mismatched identities, links,
hard links and oversized content are refused. No native Agent support is
inferred from `loadSession`, a directory name or an Agent-supplied claim.

One immutable recovery point per Session is published below the root-owned
`retained-recovery` directory, mode 0700. Only the two selected files and bounded
evidence metadata are copied; unrelated home/workspace files, credentials and
live grants are not recovery material. Evidence binds the exact launch,
operation, integration digest, content digest and original expiry. Identical
retry revalidates the retained bytes, even after source loss; conflicting retry
cannot replace the point or renew expiry. Neither an expired point nor a
partial/corrupt record yields evidence. Expiry rejects use; this component does
not delete Forensics or implement the broader retention janitor owned by
`louiselm-d6fv.5.5`.

The asynchronous mechanical API does not itself authorize registration,
Disposal or Resume. `.6` must bind its response to the authenticated supervisor
channel and current canonical lifecycle state. `.2` owns controller-loss
settlement and operator-only reconstruction into a fresh authorized Session.
The VM test restores a counter in a fresh measured fixture with a reused UID;
it does not establish a vendor ACP recovery contract or desktop availability.

## Lifecycle authority

`InstalledBroker::request_lifecycle` authenticates outcomes with the installed
launcher verifier and uses the existing supervisor lifecycle protocol.
`LifecycleCaller` is a trusted Rust boundary, deliberately not a deserializable
wire role. Operator UIDs must come from authenticated local transport.
Coordinator membership, descendants, revision and expiry must come from trusted
Run policy; Agent-supplied lists do not establish that policy. Agent capability
channels cannot request lifecycle actions. Only operators can Resume.

The broker persists exact request bytes and caller identity before dispatch.
The existing protocol checks expected state, receipt sequence and envelope
revision. A Session can have only one unresolved broker intent. Completed
requests replay their original signed receipt; conflicting request-ID reuse is
refused. Correlated supervisor refusals are durable outcomes too, so a failed
mechanic does not strand the pending intent across restart. Transport failure
is uncertain and must not be recorded as a completed refusal.

Signed authorized outcomes must match a persisted intent before the broker
acknowledges them. The supervisor still owns process mechanics, signing, actual
Agent identity and grant-lifetime revocation. The broker never reconstructs
process authority from a PID, Session membership or a status message.

`InstalledBroker::quarantine` persists the quarantine condition, stops command
approvals through the existing command owner, and requests Park. Its signed
Park outcome proves the mechanical transition. The durable quarantine marker
prevents subsequent Resume. Failed revocation or uncertain transport closes the
connection; that closure alone does not prove process cleanup.

## Ordered Attention projection

The broker owns an `Outbox` under its authorization state directory. Producers
bind normalized conditions to authenticated Session/Run identities and retain
one operation UUID for each unresolved condition. A later failure after
resolution needs a fresh operation UUID. Repeated enqueue identities require
identical changes.

`AttentionEndpoint` publishes directly to capture-service through the local
Attention socket. It checks the receiver's kernel UID before sending the
Attention producer capability. The capability file must be a private regular
file owned by the broker UID. Installation must provision socket access and
that broker-owned Attention-only capability; it never belongs in a Session.
The existing operator observer and ordinary Attention mutations remain usable
without a broker projection cursor.

Capture-service accepts an authenticated `project` request, validates its
closed schema, and commits the condition and sequence cursor atomically.
Projection-created conditions are eligible for delivery without Neovim.
Only the next sequence can advance state; identical retries are acknowledged,
current-sequence conflicts and gaps fail, and stale entries never recreate
cleared conditions. The digest covers sorted-key canonical projection JSON.
The shared fixture is `tests/fixtures/broker_attention_projection.json`.

`InstalledBroker::deliver_attention` owns one delivery attempt on a broker
worker. The transport uses an asynchronous completion and an owned thread,
with a 30-second read deadline and bounded frames. The oldest entry stays
pending until its exact sequence/digest acknowledgement is durable. Delivery
failure or compromised projection output cannot authorize a lifecycle action,
rewrite launcher receipts or change capability policy.

The initial outbox retains at most 4096 entries and refuses further enqueue at
that bound. History is not silently truncated and sequences never reset on
restart. Lifecycle request history has the same explicit bound per Session.

## Verification

`skills-core/tests/broker/lifecycle.rs` covers authorization, coordinator scope,
CAS races, restart/replay, refusals, signed-outcome binding and socket-level
quarantine ordering. `skills-core/tests/broker/attention.rs` covers outbox
ordering, retry identity, validation and authenticated socket acknowledgements.
`capture-service/tests/attention.rs` covers receiver restart, stale delivery,
gaps/conflicts, capability authentication and the shared wire fixture.

`launch_supervisor/recovery_tests.rs` covers selected bytes, binding/replay,
expiry, corruption, links, interrupted publication and pinned-directory sealing.
The existing `privileged_measured_agent_owns_isolated_tool_lifecycle` VM gate now
also saves/loads the deterministic checkpoint, rejects retention while running,
tests failed sealing, and denies the recycled UID access to old source/retained
data. The ordinary production adapter's unset layout is explicitly refused.
Host-identity fixture executables must be staged in a traversable root-owned
test directory, not below a user's mode-0700 home; do not weaken home permissions
to run this gate.

Run both owning crates' complete gates from `docs/agent-testing.md`. These
tests do not establish installed desktop/vendor cutover or a fully Verified
Session; that acceptance remains in `louiselm-d6fv.9`.
