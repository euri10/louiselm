# Broker lifecycle and Attention delivery

`louiselm-qbr.5.1.2` owns the full recovery/status integration. This document
describes its implemented authorization, reattachment and projection boundaries.
Operator reconstruction is implemented for the measured synthetic recovery layout.
`BrokerService::session_status` composes the canonical `SessionStatus` consumed
by the installed operator CLI and authenticated Agent self-status relay. These
surfaces do not enable the maintainer's ordinary editor path to create installed
broker Sessions or establish vendor recovery support. The confirmed design in
`louiselm-c0vs` requires broker-owned
recovery evidence backed by trusted supervisor retention proof; a reference
string alone must not authorize disposal. `louiselm-qbr.5.1.2.5` supplies the
retention mechanics below. `louiselm-qbr.5.1.2.6` connects them to authenticated
controller registration and broker recovery admission.

## Serving one rendezvous

`BrokerService::serve_connection` accepts one supervisor and routes it by the
connection's first packet: a launch request starts a new launch, a reconnect
offer reattaches. A running broker cannot know which is arriving, and the two
entry points below each demanded a specific opening packet, so neither could
serve a live rendezvous alone. Any other opening packet is refused without a
response, because nothing correlates one to an unrecognised packet.

`serve_launch` and `serve_reconnect` remain as the single-purpose entry points
the tests drive; all three share the same post-accept transactions.

`louiselm-control serve` runs the installed broker under its dedicated non-root
UID/GID, with no additional group authority, using `/var/lib/louiselm/broker` as its
machine-lifetime state. It also supports read-only `session inspect ID --json`
and explicit offline `adopt-state --confirm`; none accepts caller-selected paths.
The executable must belong to the configured trusted release. Startup
checks the installed authority, private directories and durable identity marker
before opening the broker stores. The supplementary list may repeat the primary
GID, as systemd initializes it; any other GID is refused.

One Control broker serves exactly one operator identity, enforced by
`InstalledBroker::authorize` and `LifecycleCaller::Operator`. Multiple operators
would require separate broker instances, identities, sockets and state; a
partitioned multi-operator broker is not supported.

The daemon separates `accept_connection` from `serve_accepted`: each accepted
supervisor gets its own handshake and continuing `step` worker. A silent or
invalid peer cannot hold up another Session's launch. Handshake and operation
response deadlines remain bounded; waiting for the next unsolicited packet on
an idle Session has no deadline. Peer closure still wakes that worker.

SIGTERM uses Linux's native process termination action. Process teardown closes
all Session descriptors promptly, including workers blocked in handshake or
storage I/O. There is no drain, worker join or synthetic acknowledgement on stop.
Only the existing durable-before-ACK receipt path can acknowledge an outcome;
the supervisor observes Broker loss and reattaches through the same retained
manager listener after restart. The system units below provision the listener;
inspection does not authorize new work. Its independent
Attention worker delivers queued projections without waiting for a Session
connection.

`SeqpacketListener::adopt` accepts an owned, listening Unix `SOCK_SEQPACKET`
descriptor. `inherited_descriptor` validates that `LISTEN_PID` names this
process and `LISTEN_FDS` names exactly one descriptor. The daemon must acquire
ownership of that descriptor at its process-entry boundary; parsing alone does
not acquire it. Adoption applies and verifies credential/buffer settings and
sets close-on-exec. Listener cancellation wakes the accept worker without
shutting down the socket shared with the service manager, so the manager's
descriptor remains usable across broker close and restart.

`BrokerService::over` composes the existing stores over that listener.
`InstalledBroker::over` additionally checks its kernel-reported path against
the installed rendezvous and uses the same identity and private-directory
checks as `bind`.

`connect_control_broker` and its reconnect path use
`SeqpacketConnector::connect_via_manager`: the listener's `SO_PEERCRED` may
name the installed broker or the root system service manager, while every
packet's `SCM_CREDENTIALS` must name the installed broker UID/GID alone.
An inherited listener retains its creator's peer credentials even when another
identity accepts it; socket ownership is not message authority. Ordinary
`SeqpacketConnector::connect` still requires one pin for both observations.

The socket unit must enable `PassCredentials=true` before connections can queue
packets during broker downtime. Adoption also asserts and verifies `SO_PASSCRED`;
setting it after a packet was queued cannot recover missing credentials.
The VM gate in `installed_socket_tests.rs` uses a root-created listener with
separate non-root worker processes and exercises the production connection and
reconnect paths. It rejects both root and unrelated-UID senders even when they
hold the accepted socket. This is a credential-boundary test. The separate
`installed_daemon_tests::privileged_activated_daemon_serves_launches_and_restart`
gate executes the actual installed binary in a disposable VM's private mount
namespace. It covers simultaneous launches beside a silent peer, storage
failure without an ACK, SIGTERM, retained-supervisor restart, and startup
identity/directory/marker refusals. Attention coverage includes absent endpoint
configuration, receiver outage during launches, pre-start and running enqueue,
wrong-ACK retry, and interrupted delivery retried after restart without a new
Session connection. It requires `LOUISELM_REQUIRE_CONTROL_DAEMON=1`
and `unshare --mount --propagation private`; ordinary Cargo skips it.

## System service installation

`skills-core/contrib/systemd/louiselm-broker.socket` and
`louiselm-broker.service` are **system** units. They require a trusted installed
release containing `louiselm-control` and an existing, dedicated non-login
`louiselm-broker` account/group whose UID/GID match the installed launcher
configuration. It must not be the operator, capture-service, or a Session
identity, and must have no additional groups. Do not use a dynamic account:
broker state binds the machine-lifetime UID/GID. For an installation using a
different account name, override `User=`/`Group=` in both units and
`SocketUser=`/`SocketGroup=` in the socket unit to that same installed identity.

From the repository root, after installing that release and identity:

```sh
sudo install -m 0644 skills-core/contrib/systemd/louiselm-broker.socket \
  /etc/systemd/system/louiselm-broker.socket
sudo install -m 0644 skills-core/contrib/systemd/louiselm-broker.service \
  /etc/systemd/system/louiselm-broker.service
sudo systemd-analyze verify /etc/systemd/system/louiselm-broker.socket \
  /etc/systemd/system/louiselm-broker.service
sudo systemctl daemon-reload
sudo systemctl enable --now louiselm-broker.socket louiselm-broker.service
```

The socket owns `RuntimeDirectory=louiselm` at mode 0700 and creates
`/run/louiselm/control.sock` at mode 0600, both broker-owned. Its
`ExecStartPre=/usr/bin/true` starts the execution context that provisions the
runtime directory before binding. `PassCredentials=true` applies before any
packet can queue. The service owns `StateDirectory=louiselm/broker` at mode
0700; systemd leaves the intermediate `/var/lib/louiselm` root-owned, satisfying
the installed broker's ancestor checks. The service also owns the separate
`RuntimeDirectory=louiselm-operator` at mode 0755. The daemon creates
`/run/louiselm-operator/inspect.sock` there at mode 0666: peers can connect to
receive a typed refusal, but only the installed operator UID reaches a request
read or Session lookup. This directory must not replace the private supervisor
rendezvous directory.

Both units are enabled: the service runs even without new connections, so
Attention delivery and reconciliation continue. A crash restarts it after
250ms. Stopping or restarting only the service preserves the socket inode and
runtime directory; stopping the socket releases its runtime directory. Durable
state survives either stop. Do not move the supervisor rendezvous directory to
the service, or delete broker state during unit upgrades. The separate inspection
directory is service-owned and is recreated on service start. Startup rechecks identity,
directory permissions, release authority and the durable identity marker.

`sudo env LOUISELM_REQUIRE_BROKER_SYSTEMD=1 python3 scripts/test-broker-systemd.py`
is a disposable-VM-only gate, also run in privileged CI. It exercises these
unit files with real PID 1, substituting temporary paths, an unprivileged test
identity and a socket probe for the daemon. It verifies enablement, ownership,
credential-bearing queued packets, idle crash restart, socket preservation and
durable state. The separate installed-daemon gate above covers the measured
binary, launch/reconnect, additional-group refusal and durable receipts; neither
gate claims acceptance in the maintainer's live editor.

## Durable broker identity

Both installed constructors check `identity.json` before opening authorization,
receipt, lifecycle, audit or outbox stores. The closed
`louiselm.broker-identity/1` record binds the broker UID and GID, independently of
the current release. First startup requires an empty state directory and
publishes the mode-0600 marker without replacement, syncing bytes and the
directory before proceeding. A matching restart preserves the marker bytes.

Changed identity, existing state without a marker, malformed or oversized
records, links and non-private marker files fail closed. No broker store is
read or repaired after these refusals. An interrupted initial publication may
leave an unmarked nonempty directory, which also refuses automatic adoption.
Identity mismatch names the explicit adoption command. Missing or corrupt
markers require inspection/restoration; adoption never invents their prior identity.

This marker detects accidental identity reassignment; it is not tamper-proof
against the state owner or root and does not replace signed receipt validation.
The accepted machine-lifetime path is `/var/lib/louiselm/broker`, provisioned by
the system service above.

For a deliberate broker UID/GID change, update the installed authority and unit
identities first. Let systemd provision/re-own the state under the new identity;
the unchanged old marker still refuses startup. Stop both broker units before
adoption, then run the installed, measured executable from the configured
operator's account using existing administrative sudo permission:

```sh
sudo systemctl stop louiselm-broker.service louiselm-broker.socket
sudo -u louiselm-broker -g louiselm-broker \
  /usr/local/lib/louiselm/current/bin/louiselm-control adopt-state --confirm
sudo systemctl start louiselm-broker.socket louiselm-broker.service
```

Substitute the configured broker account if renamed. The command requires the
actual broker UID/GID without additional groups and sudo's canonical `SUDO_UID`
matching the installed operator. Root execution, a foreign operator and absent
confirmation refuse. No automatic sudo permission is installed for this verb;
it does not run through an Agent or the launcher's `run` permission. This trusts
sudo and the broker account, not arbitrary environment input from a Session;
root and the state-owning broker can already alter the marker directly.

The installed broker holds an exclusive directory lock for its lifetime.
Adoption takes the same lock without waiting, refuses active state, and does
nothing if the marker already matches. A changed marker is published atomically
only after a mode-0600 `identity-adoptions/<milliseconds>.json` audit record and
its directories are durable. This machine-scoped `StateIdentityAdoption` decision
names the operator and previous/new UID/GID without inventing Session identifiers.
It records authorized intent: a crash before marker publication can leave that
record alongside the old marker. Preserve it and retry explicitly; an existing
audit filename is never overwritten. Matching retries leave the marker and audit
untouched. Receipt bytes and authorization records are neither rewritten nor
repaired; ordinary receipt-chain verification still applies after restart.

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

### Unverifiable Session history

Installed inspection rechecks stored receipts against their original registered
identity and current public verification authority. Recovery, status, lifecycle,
promotion and continuing broker packets use the same history refusal boundary.
An unreadable, missing recovery chain, malformed, contradictory or unverifiable
history quarantines only its Session. Its exact receipt files are preserved;
links and special receipt files are refused without following or blocking on them.

The broker records a durable normalized failure under `authorizations/history-failures`,
uses the existing lifecycle quarantine, and enqueues `SessionFailed` through the
Attention outbox. Inspection returns the canonical `ReceiptChainInvalid` error
with `InspectReceiptChain` as its next action, without inventing a process state
or returning untrusted measurements. Restoring old bytes or retrying after restart
does not clear that decision. Reporting failures still refuse the operation;
retry completes the existing projection without changing its identity.

The original worker closes its channel on history failure, withdrawing further
broker effects and triggering existing supervisor Broker loss handling. This
does not itself prove that the process tree has frozen or been disposed. Other
Sessions can still inspect and reconnect. Invalid shared public-key authority
remains a broker-wide refusal, as does an invalid identity marker at startup.
The installed isolation gate uses real signing authority and a dedicated broker
UID to exercise these distinctions; it does not establish live vendor cutover.

### Retired private-key completion

The root-owned keyring records every admitted Session's signing lifetime.
The supervisor retains its reference through Park/resume, broker loss and
pending terminal receipts. It completes the reference only after proven
process/identity cleanup, relay quiescence and exact terminal receipt ACK,
after disconnecting its event receiver. Unproven exits leave a live reference.

Completion, signing, admission and rotation share the install lock. Once a
retired key has no live references, cleanup durably forbids further signing,
unlinks only that private key and fsyncs its directory. Historical public keys,
bindings and exact receipts survive restart and upgrade. Missing authority or
stale references never establish completion; revoked keys are excluded from
routine cleanup. See [maintenance and acceptance limits](../skills-core/README.md#retired-launcher-private-keys).

### Compromised launcher keys

An explicit trusted administrator `launcher revoke-key --expected-key-id <id>`
persists compromise separately from retirement. The broker refuses affected
history with `SigningKeyRevoked` and `ContactOperator`, reuses durable lifecycle
quarantine and the Attention outbox, and blocks recovery and continuation.
All receipts under the key are untrusted, irrespective of their claimed age;
their bytes are retained. An old signature or stale status cannot undo revocation.

The installed supervisor independently observes root-owned authority changes,
revokes capabilities before freezing, and retains a local control observation
separately from receipt history. This works even without the broker. Required
mechanical failure remains visible as `failed`; missing/unreadable observation
means containment is unconfirmed. `InstalledBroker::key_revocation` reports the
original key binding and this local observation without verifying compromised
receipts or presenting them as containment evidence. Observations do not prove
current liveness, restore trust, or grant Resume. See the
[administrator command and trust limits](../skills-core/README.md#compromised-launcher-keys).

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

## Operator inspection

As the installed operator, without sudo:

```sh
/usr/local/lib/louiselm/current/bin/louiselm-control session inspect SESSION_ID --json
```

Success exits 0 and writes exactly canonical `louiselm.launch.session-status/5` JSON to
stdout, without prose or an added newline. Refusals leave stdout empty and write
`louiselm.operator-error/1` JSON to stderr, with `error` and `next_action`:

| Exit | Error | Next safe action |
| --- | --- | --- |
| 2 | `invalid_request` | `check_request` |
| 3 | `broker_unavailable` | `check_broker_service` |
| 4 | `authentication_refused` | `use_configured_operator` |
| 5 | `unknown_session` | `check_session_id` |
| 6 | `status_unavailable` | `retry_inspection` |

The CLI verifies its installed release and pins the daemon's kernel UID before
sending any Session ID. The daemon pins the client's kernel UID before reading
the request or touching Session state. Root, the broker account and Session
identities are not substitutes for the installed operator. This is a separate
daemon-created Unix stream socket, not the manager-created supervisor socket;
its peer credentials therefore identify the actual broker. Frames and queues
are bounded, with a 35-second total exchange deadline and a 30-second worker
reply budget. Reads grant nothing and require no new sudo permission.

Each Session worker owns all receives. It services a bounded inspection queue
between supervisor packets, using non-consuming readiness waits while idle;
operator queries never race another reader or hold a shared registry lock over
I/O. Busy or disconnected Sessions return `status_unavailable`. After restart,
inspection resumes when the retained supervisor reattaches. A known Session
without a live owner also returns `status_unavailable`, even if durable receipts
exist: old evidence is not current authenticated mechanical status. An unknown
Session returns `unknown_session` only after operator authentication.

## Canonical status composition

`BrokerService::session_status` reads authenticated mechanical state through the
existing supervisor channel, then adds the broker-owned fields the supervisor
must not author: detailed Verified posture, retained recovery readiness and the
lifecycle actions this caller may currently request.

`LifecycleCaller::allowed_actions` derives that action set from one predicate
shared with `LifecycleCaller::permits`, so status cannot advertise a mutation the
next request would refuse. It reports the mechanically valid transitions out of
the current state, narrowed by caller scope: only operators see Resume, a
coordinator sees only actions on a descendant inside its unexpired envelope, and
an Agent capability channel sees none at any state. A durable quarantine marker
withdraws Resume and leaves every unrelated action intact. A serialized operation
still in flight withdraws all of them, because the status schema rejects a
pending operation advertised alongside an executable action.

`SessionStatus.posture` is the broker-derived, display-only `PostureStatus` in
`louiselm.launch.session-status/5`. Status callers supply no posture verdict.
Each response includes all six dimensions in canonical order, with state,
requirement, bounded evidence references, a typed failure and fixed next action,
and freshness. The aggregate must agree with the dimensions; `Pending` is valid
only during actual initialization. A missing producer is `unverified` with
`evidence_missing` in its failed dimension. A waived dimension never counts as
fully verified. Parsing this record cannot construct trusted `Posture` inputs.

The first producer uses the exact authenticated launch/start receipt chain,
bound to the authorized Session, Run, request, revision, installed identity,
release and signing key. The existing receipt store retains original admission
history. The broker loads a private runtime-evidence record at launch or
authenticated reattachment, using the original successful launch-proof audit
time. A missing audit observation has no fabricated success timestamp and
leaves runtime unverified. Isolation and network still need their own producers.

Signed conformance admission additionally binds exact canonical observations
retained by the receipt store. Digest-bearing receipts cannot be acknowledged
without their matching report, and report damage refuses history use and
reattachment. The supervisor transfers bounded, ordered report fragments on the
same authenticated connection before the durable ACK. Gate activation comes
only from protected launcher policy; the broker authorization carries attendance
and exact operator-waiver bindings, never an activation override. The posture
owner retains a bounded `conformance_report` reference
for isolation; it is historical evidence, never primary proof of current host
conformance. Missing current measurements keep the dimension unverified and
retain no invented successful-check time. Raw observations are excluded from
status. The admission and report retention section in
`docs/launcher-conformance.md` describes storage, inspection and pre-cutover limits.

Supply producers construct non-deserializable `SupplyEvidence` from the exact
authenticated Session inputs and matching `DiscoveryProof`, using the existing
supply validators and protected store, policy and registry. This blocking work
belongs on the producer worker. It verifies the managed Generation/view and
policy, the admitted Agent/runtime registration, native controls and the fixed
Provider disclosure independently; it supplies no new runtime verdict.
`InstalledBroker::retain_supply_posture` accepts these bounded facts only for
the worker's authorized request and exact sequence-zero receipt. Foreign,
future, duplicate-time and out-of-order results cannot replace retained facts.
Missing inputs or proof leave the relevant dimensions unverified.

Only references, typed outcomes and observation times enter retained posture;
raw manifests, Provider configuration and instruction contents do not. A later
failed check keeps that dimension's last successful reference/time while
reporting its current failure. Supply and disclosure describe frozen admission
inputs; ordinary updates for future Sessions do not reselect this Session's
Generation or rewrite its signed receipt. Native controls additionally require
the original live confinement and connected broker. Quarantine or a clock before
the observation invalidates the retained proof. These facts have no invented
periodic timeout, and status reads never rematerialize or remeasure them.

Supply facts currently belong to the connected broker Session owner. After
restart they remain explicitly missing until the trusted producer supplies
evidence for the exact original admission again; current preflight selection
cannot stand in for it. This component does not create a vendor observation
source: installed source wiring and actual adapter discovery controls remain
part of `louiselm-d6fv.9`. In their absence, status stays truthfully partial.

Runtime freshness has a `launch` basis: it describes the checked launch proof
for the original supervised Agent lifetime, not a new executable measurement or
continuous conformance assertion. Current terminal/disconnected/quarantined
state, or a clock preceding the recorded check, cannot keep that proof verified;
the original check time and evidence remain inspectable as `invalidated`, with
the typed reason `evidence_invalidated` rather than `evidence_missing`.
The canonical status boundary rejects a live-state claim against a terminal
durable receipt even when its head digest matches. Mechanical progress may
precede a new receipt; terminal history can never be undone by a status reply.
Reading status and restarting the broker do not refresh the proof time. The
status path still reads authenticated mechanical state but runs no evidence
probes, writes no posture decisions, resets no budget and grants no capability.

Launch permission expiry limits initial consumption, not the lifetime of an
already measured runtime. Broker-loss grace remains the supervisor's signed
interval: status consumes its connection state and ordered receipts without
starting a timer. Reattachment before grace expiry may restore the same
launch-backed runtime proof; after signed Park it preserves Park until an
authorized Resume. Neither path restores command grants or recovery admission.
Restart revalidates the exact signed chain and original audit timestamp;
unreadable or corrupt proof refuses reattachment. Conformance deadlines,
waivers and quarantine continue to belong to their source-specific producers;
missing producers remain explicitly unverified, with no synthetic expiry.

The maintainer-confirmed design is `louiselm-rn38`. Supply/disclosure producer
integration is `louiselm-d6fv.6.11`; network integration remains `.6.13`;
conformance, waiver and quarantine owners retain their existing policies. This
partial status path does not establish the installed Verified cutover in
`louiselm-d6fv.9`.

`SessionStatus.recovery` comes from the broker's durable `recovery_readiness`
record, identically for operators and the scoped Agent. It reports `ready` with
the immutable operation ID and original expiry, `expired`, `quarantined`, or
`unavailable` with `evidence_missing` or `pending_durability`. Missing evidence
includes unsupported Agent layouts and unavailable `loadSession`; advertised
support alone creates no evidence. Pending registration or revalidation cannot
reuse an earlier ready record. Malformed or foreign evidence fails the status
read instead of becoming readiness. Reads grant nothing, renew no expiry, and
do not change ordinary versus required-cold-recovery admission. The record has
no storage paths, ACP identity, raw evidence or capture-service projection.

The authenticated Agent capability channel accepts `louiselm.launch.command/1`
with `operation.kind = "status_request"` and the usual request ID, Session, Run
and envelope revision. The supervisor relays it on the retained broker channel;
`BrokerService::step` dispatches the read when its worker is idle.
`BrokerService::serve_agent_status` is the dedicated single-read entry point.
Both enforce self-scope against the Session's own
authorization and refuse any other subject with `SubjectMismatch` — identically
whether that Session is a live sibling or was never authorized, so a refusal
cannot be used to probe for other Sessions. The answer is the same composition
the operator reads, differing only in the caller-scoped action set, which for an
Agent is always empty. The reply operation is `status_result` with the canonical
`SessionStatus` in `status`, or `status_refused` with a stable `error` code. A
refusal carries only the caller's own subject binding, never the requested
foreign identifier, mechanical state or operator evidence. No command approval
or recovery admission is required to read status on an enabled Agent channel.

The supervisor keeps one pending read and uses its own correlation sequence,
restoring the Agent's request ID on reply. A 60-second relay deadline bounds a
missing reply; status reads neither advance effect sequences nor spend or renew
command authority. Late replies and deadlines cannot complete a newer read,
including after Resume. Tools and delegated helpers cannot request self status.

A status request arriving while the worker already holds another operation is
refused as retryable `OperationPending` by `refuse_nested_status` rather than
served out of order: answering inline would arm a receive competing with the one
already waiting. Note that Park disables the capability channel, so the Agent
path is unreachable while Parked; the operator path still reports the parked
state and offers Resume.

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

`louiselm-control serve` owns one delivery loop for the process lifetime. It
drains successful deliveries in order, polls an empty outbox every second, and
retries failures with exponential delays of 1, 2, 4, 8, 16, then at most 30
seconds. Each transport attempt retains its existing deadline. Session accept
and operation workers never wait for delivery. SIGTERM terminates delivery I/O
with the process; an interrupted attempt receives no synthetic ACK and remains
eligible for exact retry after restart. The receiver's durable sequence cursor
makes a retry safe when it applied an entry before the broker stopped.

Before each attempt the worker reads `/etc/louiselm-broker-attention.json`:

```json
{
  "socket": "/run/louiselm-attention/project.sock",
  "capability_file": "/var/lib/louiselm-attention/producer-capability",
  "receiver_uid": 1000
}
```

This closed record permits only these three required fields. It must be a
root-owned regular file, not a symlink, not group/world writable, and at most
4096 bytes. Both paths must be absolute. The receiver UID is checked against
the connected peer before the private capability is sent. No secret belongs
in this configuration file. Missing or invalid configuration leaves the worker
retrying and Sessions usable; provisioning or replacing the record takes effect
on a later attempt without a daemon restart. Diagnostics report the outage once
and recovery after an actual delivery, without endpoint contents, capabilities
or peer responses. If placing the capability under the broker state directory,
initialize its identity marker on empty state first, then provision the capability.

Capture-service exposes projections and read-only Run lifecycle facts on a separate endpoint. It reads
`/etc/louiselm-capture-broker.json` at startup: a closed, root-owned regular record
that is not group/world-writable, containing `socket`, `broker_uid` and
`capability_sha256`. The
installed socket path is fixed to `/run/louiselm-attention/project.sock`. Missing
policy disables that endpoint; malformed or insecure policy refuses startup.
There is no producer token in the receiver's state or configuration.

The root provisioner derives both identities from the installed launcher's
`public-config.json`: the dedicated broker UID/GID and the operator UID/GID
running capture-service. After updating capture-service and its user unit, run
from the repository root:

```sh
sudo python3 scripts/install-broker-attention.py
systemctl --user daemon-reload
systemctl --user restart louiselm-capture.service
```

Provisioning creates a 256-bit credential at the broker-owned mode-0400 path
above, outside broker state so its first-start identity marker remains intact.
Only its SHA-256 enters the receiver policy. The sender policy contains the
receiver's kernel UID and credential path. Repeated provisioning verifies
existing bytes/ownership without rotation; mismatched or partially written
files refuse automatic repair. Preserve those files and inspect the installed
identities before explicitly replacing configuration.

The provisioner also installs a tmpfiles rule recreating the mode-0711,
capture-owned `/run/louiselm-attention` on boot. The user unit grants write
access to this directory through `ProtectSystem=strict`. The projection socket
is mode 0666 to permit cross-user connection without shared groups or ACL tools;
capture-service checks the peer's kernel UID before reading or sending data,
then checks the distinct producer credential before accepting `project`.
Unknown UIDs are disconnected without a snapshot. Frames are limited to 64 KiB,
connections to 32 concurrent clients, and requests to a two-second deadline.

The existing operator `attention.sock` remains mode 0600, supports its ordinary
mutations and observer snapshots, and rejects `project` even with the operator
token. The producer token cannot authorize those ordinary mutations or Run
lifecycle operations. Its `run_lifecycle` query returns only the exact Run UUID,
durable storage revision and closed lifecycle state; missing/disabled Run storage
is a refusal, never evidence that a Run is alive. The storage revision is not a
capability-envelope revision. The dedicated endpoint has no snapshot or ordinary mutation verbs.
Neither endpoint grants lifecycle, signing or capability-policy authority.

### Durable Skill Admission requests

`GrantRequest.skill_requests` is an optional, explicit permission on the existing
broker launch authorization: sorted intended Agent names, `allow_run`, and an
absolute `expires_at_ms`. Unset grants no request permission. Cold reconstruction
does not inherit this permission into a replacement Session.

The authenticated Agent channel accepts `skill_request` with a stable nested
`request_id`, subject kind (`session` or `run`), sorted unique canonical package
digests and intended Agent names. The supervisor owns relay correlation; the
broker derives all subject IDs and the envelope revision from its retained
authorization and checks current Session mechanics and permission lifetime.
A new Run request also requires a current authenticated lifecycle observation.
The Agent supplies neither an approval nor a foreign subject ID.

Before a successful reply, the broker persists exact content, a random stable
operation UUID and the existing outbox's `skill_approval_pending` intent with
fixed `admission_required` code. Packages, Agent names and prose never enter that
projection. Exact retries retain the operation and terminal outcome; changed
content requires a fresh request ID. Accepted retries need no new receiver
connection. No discovery scan or standalone Admission creates a request.

Use the existing UID-authenticated operator endpoint (no sudo or live editor):

```sh
louiselm-control skill-request inspect OPERATION_UUID --json
louiselm-control skill-request reject OPERATION_UUID --json
louiselm-control skill-request cancel OPERATION_UUID --json
```

Reject/cancel persist an immutable terminal outcome and enqueue an exact clear
before replying. Contradictory terminal decisions refuse; explicit resubmission
uses a fresh operation. Signed Session termination cancels only that Session's
requests. Capture-service's durable `disposed` Run fact cancels only that Run's
requests; Session termination never substitutes for Run termination. Park retains
both. The independent delivery worker reconciles terminal subjects after restart
and repairs interrupted projection intent. Unavailable Run facts retain pending
state and cannot block other subjects' local cleanup.

This path grants no Admission, Resume, envelope expansion or Session replacement.
The cross-crate gate
`python3 scripts/test-skill-requests` exercises the actual broker handler/outbox
and disposable capture-service through separate processes, including broker and
receiver restart, Park and Run disposal. Crate tests cover policy/refusal,
operator decisions, storage crash windows and late relay replies. These gates
do not certify the maintainer's installed vendor launch path.

### Resolving a request through Skill Admission

The administrator may explicitly enable read-only evidence access:

```sh
sudo python3 scripts/install-broker-admission.py --store /var/lib/louiselm-skills
```

The store must already exist, have trusted-release provenance and enrolled trust,
and be owned by the installed operator. Keep private signing keys outside it.
Ancestors must not be writable by other identities; the broker also needs traversal
access. A private home is not a suitable shared-store location. Provisioning reads
installed identities, shares only package/public trust/Generation evidence with
the dedicated broker group, and pins the exact store and trust domain in root-owned
`/etc/louiselm-broker-admission.json`. Staging inherits the group without allowing
broker traversal. Atomic trust/Generation replacements preserve opted-in read
permissions; ordinary stores retain private files. No store is promoted, no key is
enrolled, and no signing secret is shared. Stop writers while provisioning.

With that configuration installed, the operator can run:

```sh
louiselm-skills generation admit --store /var/lib/louiselm-skills \
  --member PACKAGE_DIGEST:read=codex --key /private/signing-key \
  --skill-request OPERATION_UUID --robot-json
```

Members and literal Agent scope must exactly match the request. The initial linked
path uses the embedded policy; caller-selected policy or trust roots cannot expand
it. The operation UUID is covered by the Generation signature. Repeating the same
linked ceremony recovers those signed bytes and completes uncertain registration
without signing again; changing its members or review depth refuses.

The broker's independent reconciliation worker reads the configured evidence under
a shared read-only trust lock. It verifies the signature, persisted approval index,
exact packaged bytes and Agent scope, and confirms file/directory durability. It
refuses an activation recovery journal; only the skills tool can repair supply.
It then persists the approved Generation identity, terminal outcome and exact
Attention clear intent before reporting approval. Delivery outages leave the clear
queued, and restart reconciles the same operation. Reject/cancel and authoritative
subject-end records remain terminal even if Admission subsequently finishes.

`approved` means signed and persisted, including `pending_witness`; it does **not**
mean witnessed, activated, usable or fully Verified. Existing Instruction views,
launch authority and operator Resume remain unchanged. Operator inspection remains
read-only. CLI output therefore may initially report `pending` or an unavailable
broker alongside a successful global Admission; retry inspection, not a new request.
Unlinked `generation admit` keeps its original output and needs no broker or Attention.

`scripts/test-broker-admission.py` runs the actual CLI and broker handlers across
distinct UIDs inside an explicitly enabled private mount namespace. It covers
unset/standalone operation, opt-in read-only provisioning, broker loss after
preflight, and restart resolution without access to a signing key. It uses disposable
software keys and test-only provenance, not hardware or live desktop acceptance.

`scripts/test-broker-attention.py` runs under a private mount namespace in a
disposable VM with `LOUISELM_REQUIRE_BROKER_ATTENTION=1` and
`LOUISELM_TEST_CAPTURE` pointing to the built capture executable. CI runs it
with root solely inside that isolated fixture. It exercises the actual CLI and
provisioner across distinct UIDs, unset policy, credential and identity denial,
idempotent provisioning and delivery, receiver restart and permission drift.
The daemon VM gate still uses a synthetic receiver for its independent lifecycle
checks; these gates do not claim installed desktop or phone acceptance.

The initial outbox retains at most 4096 entries and refuses further enqueue at
that bound. History is not silently truncated and sequences never reset on
restart. Lifecycle request history has the same explicit bound per Session.

## Verification

`skills-core/tests/operator.rs` covers UID refusal before lookup, malformed and
oversized frames, slow-peer deadlines, stale socket recovery and foreign/live
path preservation. `tests/control_binary.rs` asserts typed CLI errors and empty
stdout on refusal; the control binary's queue tests reject expired inspection
work and preserve the original exchange deadline. `tests/launch_transport.rs`
checks that idle readiness timeouts neither consume packets nor close channels.
The installed-daemon VM gate executes the actual CLI, compares its canonical
bytes with Agent self-status, checks foreign-Session redaction and distinct
errors, and inspects again after retained-supervisor reattachment. The real
systemd gate checks both runtime directories without conflating their lifetimes.

`skills-core/tests/broker/lifecycle.rs` covers authorization, coordinator scope,
CAS races, restart/replay, refusals, signed-outcome binding and socket-level
quarantine ordering. It also covers status composition: caller-scoped allowed
actions across state, quarantine, expiry and non-descendant targets, and the
composed `SessionStatus` round-tripping through `parse_canonical`. `skills-core/tests/broker/attention.rs` covers outbox
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
