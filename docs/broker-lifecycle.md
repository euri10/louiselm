# Broker lifecycle and Attention delivery

`louiselm-qbr.5.1.2` owns the full recovery/status integration. This document
describes its implemented authorization, reattachment and projection boundaries.
Operator reconstruction is implemented for the measured synthetic recovery layout;
the operator CLI and Agent self-status remain open work. The confirmed design in `louiselm-c0vs` requires broker-owned
recovery evidence backed by trusted supervisor retention proof; a reference
string alone must not authorize disposal. `louiselm-qbr.5.1.2.5` supplies the
retention mechanics below. `louiselm-qbr.5.1.2.6` connects them to authenticated
controller registration and broker recovery admission.

## Broker restart

`InstalledBroker::serve_reconnect` accepts an authenticated supervisor on the
existing rendezvous. It loads the already-consumed launch, re-verifies every
stored signature and binding, and re-establishes durability before returning
the exact checkpoint. Equal prefixes reattach; a supervisor-ahead suffix is
accepted only as continuous, valid signed bytes ending at the offered digest.
Missing interior files, unknown entries, foreign bindings, conflicting heads,
broker-ahead state and invalid signatures fail closed. No history is discarded,
spliced or repaired. Known-Session refusals quarantine locally and enqueue
normalized Attention; storage failure cannot produce an acknowledgement.

The returned Session worker has no reconstructed command/grant authority or
recovery admission. It can receive signed lifecycle outcomes and settle control
state, but it does not reset spent budgets or automatically Resume a Park.
The original short-lived launch authorization is not consumed again or renewed.

## Controller loss

The installed broker worker accepts the supervisor's authenticated loss request
only at its exact, reverified durable Park head and original Session/Run/revision.
The supervisor freezes and revokes first, retaining its unresolved loss latch
until settlement succeeds. A Session already Parked uses that existing truthful
head rather than emitting a Parked-to-Parked receipt.

The broker discards the old command approval owner, persists one immutable loss
decision, and durably enqueues normalized local Attention before acknowledging
Disposal. A current broker-owned retained point records recoverable cold Park;
missing, expired or quarantined recovery records abnormal loss instead. Unreadable
or malformed durable state is uncertain and gets no settlement ACK. The old
supervisor then disposes and seals its tree and records the terminal receipt
before releasing the host identity.

Identical requests return the original decision and Attention identity. Retry
re-establishes durability; an expired recoverable decision cannot be renewed or
replayed as usable recovery. A request conflict, store/outbox failure or uncertain
transport leaves settlement unresolved. Capture-service delivery and its ACKs
are not inputs to this decision and can retry independently after an outage.
Cold Park is a retained point, not a Running Session: fresh operator-authorized
reconstruction and its successful load/finalize remain separate obligations.

## Recovery registration and admission

### Cold reconstruction

The authenticated operator calls `InstalledBroker::authorize_cold_resume` with
the disposed source Session and a distinct target launch. The broker verifies
the source receipt chain, recoverable controller-loss settlement and retained
point. It preserves the configured Agent, Run, envelope/revision, Generation,
input manifest, recovery requirement, broker-loss policy and original deadlines.
It durably allocates the source once before issuing fresh target authorization.
Concurrent attempts cannot allocate two targets; an identical retry retains
the exact target and allowance. Interrupted authorization consumption cannot
recreate a pending authorization. A crash can require operator intervention,
but never restores spent authority.

Finite command allowance subtracts direct admitted intents (including uncertain
outcomes) and complete delegated reservations. Delegated executions are not
subtracted again. Uncapped approvals remain uncapped; zero or expired allowance
is withheld. Unavailable bounded accounting withholds that permission and
returns `withheld_command: accounting_unavailable`; independent ACP work may
continue, while governed command steps remain denied. This does not excuse
invalid overall authorization, recovery evidence, runtime or load failures.
The current command schema holds one approved command scope per Session.

The target starts with no broker command owner. On its serialized worker the
controller explicitly Parks it, calls `restore_cold_resume`, then explicitly
Resumes it to perform the exact retained ACP load. Restoration uses the existing
supervisor channel and owned asynchronous storage worker; the broker/controller
never copy protected storage. Exact restore intent and completion records bind
the target's Park head. Before acknowledging restore, the broker registers the
replacement's own retained checkpoint through the same supervisor, with the
original expiry. Its next controller loss must not depend on reusing the consumed
source allocation. Source and target integration digests differ because
they include their Session identity; verified launch receipts instead bind
the unchanged measured runtime, and the target supervisor independently checks
its supported recovery layout.

Only the trusted controller's successful load observation passed to
`finish_cold_resume` enables remaining commands after durable finalization.
Failed/cancelled load durably records failure and clears command approvals; a
later success cannot revive them. The controller retains the authenticated
channel to request Disposal or acknowledge the exited process's terminal receipt
through `step`, then drops the owner. Connection closure alone is not cleanup proof.
Successful replay does not construct another command owner. Broker restart
continues to reattach without restoring live grants or command authority.
After a pre-finalization restart, the controller must revalidate restore and
retention at the original Parked target head before loading and finalizing it;
a changed head refuses that retry instead of recreating admission.
`admit_recovery` checks the replacement's own checkpoint until its original
deadline, but admission alone never recreates capabilities. A loaded record is
historical evidence, not proof of current process liveness or lossless recovery.

This consumer supports only `louiselm.test-recovery/1`. The installed VM tests
`privileged_installed_cold_resume_{finite,uncapped,failed_load,unavailable_balance}`
check sealed-source restoration, fresh authorization, finite/uncapped enforcement,
retry, failed synthetic load and unavailable accounting. CI invokes each exact
test sequentially with its own 240-second deadline and elapsed timing. Keep them
serial because the fixtures share a dedicated test account. Ordinary Cargo runs
skip these privileged fixtures.
Vendor ACP support and desktop activation remain in `louiselm-d6fv.9`.

### Registering a retained point

`InstalledBroker::register_recovery` runs on the existing serialized broker
Session worker. Its `LifecycleCaller::Operator` identity must come from the
trusted local controller boundary and match the original launch controller.
Agent/helper channels and coordinator scopes cannot submit recovery evidence.
The closed `RecoveryRequest` binds the exact launch (including configured Agent,
Session, Run and envelope revision), durable Park receipt head, ACP identity,
operation ID and original absolute expiry. It contains no selected paths.

The supervisor checks that the same tree is idle and Parked, then calls
`RunningAgent::retain_recovery` asynchronously. Completion is returned through
the serialized owner only while its connection, process, state and receipt head
still match. Late callbacks cannot restore disposed state. The broker validates
the returned launch, operation and measured integration against its original
authorization and signed receipts, rechecks current supervisor state, and
persists the evidence before acknowledging readiness.

One immutable operation is allowed per Session. Identical retries revalidate the
protected retained bytes and return the original evidence; conflicting requests
cannot replace the point or extend expiry. A durable pending marker makes prior
evidence unavailable during revalidation and after an uncertain or failed
attempt. Torn records, foreign bindings, expiry and quarantine fail closed.
Capture-service projections and acknowledgements never enter this decision.

The trusted Run controller fixes `GrantRequest::require_cold_recovery` before
launch. Ordinary grants set it to false and remain usable without recovery or
`loadSession`. For required recovery, `InstalledBroker::admit_recovery` must
succeed before controller work dispatch; the broker independently denies command
and delegation requests until registration is durable and refuses new requests
after the original expiry. The controller may initialize the ACP checkpoint
without governed effects, explicitly Park for retention, register, and explicitly
Resume. Registration itself never Parks or Resumes. Existing command approvals
still apply; readiness is no new capability grant.

`InstalledBroker::recovery_readiness` exposes normalized unavailable, expired,
quarantined or ready state for the canonical status consumer. A ready record
proves a retained recovery point, not lossless continuation or a guaranteed
future load. Reconstructed broker Session owners start without in-memory command
admission; durable status alone does not re-enable effects.

Authorized Resume creates a new command-enforcement generation for the same
kernel-pinned Agent. It preserves the consumed dispatch floor and revoked grant
identities; pre-Park permits, helpers and late receive callbacks cannot acquire
the new generation. The deterministic fixture explicitly reconnects its socket
without automatically replaying a possibly executed command (`louiselm-fhh8`).

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
Disposal or Resume. The broker registration path binds its response to the
authenticated supervisor channel and current canonical lifecycle state. `.2` owns controller-loss
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

`tests/broker/recovery.rs` drives controller registration across the real local
transport, including controller/Agent/coordinator refusal, measured-integration
substitution, unavailable ordinary/required admission, pending revalidation,
original-evidence retry, conflicting expiry, restart, expiry and malformed records.
`recovery_dispatch_tests.rs` holds the mechanical callback and proves late
completion cannot revive terminal state. The installed broker VM gate now
initializes the measured checkpoint, proves pre-admission command denial,
registers and retries protected evidence, preserves Park until operator Resume,
executes the approved command after explicit fixture reconnect, and proves cleanup.
These tests support only the synthetic layout described above. The maintainer's
ordinary Lua Session path does not yet construct these installed broker grants;
native vendor support and desktop activation remain the Verified cutover work.

Run both owning crates' complete gates from `docs/agent-testing.md`. These
tests do not establish installed desktop/vendor cutover or a fully Verified
Session; that acceptance remains in `louiselm-d6fv.9`.
