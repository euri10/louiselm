# Explicit tool delegation

`louiselm-qbr.5.1.1.5.2` implements the Agent-command path described by
`louiselm-cross-process-tool-commit-z288`: broker-owned approval and budget,
supervisor-owned process identity and actual execution. `.5.3` extends that
path to one explicitly granted, measured isolated helper. `.5.4` composes the
installed broker and supervisor with those measured fixtures. User-visible
Verified cutover and installed vendor integration remain `louiselm-d6fv.9`.

## Cross-process Agent commands

`SystemCapabilityGate` receives a bounded `ToolExecutionRequest` only from its
pinned Agent, checking both connection and per-packet kernel credentials.
The Session owner forwards the original request and supervisor-authored
`CommandPrincipal` through the retained authenticated broker connection.
The broker never receives a kernel handle and never constructs one from a PID.
An ungranted child, passed descriptor, or first connector cannot become the Agent.

The broker persists `ApprovedCommands` with the original single-use launch
authorization: exact command digest, timeout, aggregate budget, explicit
delegation permission and absolute expiry. The signed Start receipt binds the
supervisor-proven Agent PID, assigned UID/GID and tool-isolation evidence digest.
Only these records construct the command authority, before the sequence-1 ACK.
There is no post-launch API to replace the identity or policy. Absent approval
denies command requests without ending the Session.

`InstalledBroker` checks its dedicated installed UID/GID, private state and
rendezvous directories, and root-owned public authority. It verifies signatures
with the measured OpenSSH executable. `step` handles commands and exact signed
outcomes on the original connection; it must not run beside another reader of
that channel. This adds no human prompt or Agent permission setting. Later
recovery, reconnect and controller-loss settlement remain `louiselm-qbr.5.1.2`.

`CommandAuthority` validates exact command digest, timeout, principal, revision,
expiry, request sequence and remaining budget. It spends one use and persists
normalized intent before sending `CommandOperation::Authorize`. File and audit
directory synchronization must both succeed. Failed or lost decisions never
refund budget. A spent/revoked Session cannot reconstruct fresh authority from
that audit history after a restart.

The supervisor correlates the decision with the original Agent request. Its
single-use `CommandPermit` owns that exact immutable request; the executor cannot
substitute another command. The permit rechecks the kernel lifetime, revocation
and expiry directly around the isolated process's startup gate, and the running
executor continues checking until cleanup. The decision's remaining lifetime is
anchored **before the supervisor forwarded the request**, not when the reply
arrives, so transport or audit delay can only shorten it. Principal-local
request sequences and Session-wide dispatch sequences are separate. A raw broker
`ToolExecutionRequest` or unsolicited authorization cannot execute anything.

`revoke_commands` stops broker approvals before sending revocation. The
supervisor blocks queued starts, cancels and joins the running command tree,
then reports enforcement. `command_revocation_complete` becomes true only after
that authenticated success is durably audited. Cleanup failure is not success:
the existing quarantine and identity-poisoning path applies. Local expiry stops
running work too; an earlier command timeout still wins. Revoked command authority
is not restored by the existing transport/lifecycle reconnect machinery.

The Agent receives a typed `Result`: completed output, a known pre-start refusal,
or `Unknown`. A disconnected channel also provides no proof that execution did
not happen. There is no automatic retry. Known output is delivered without
waiting for its audit acknowledgement; an audit failure cannot undo an effect.
Late authenticated actual outcomes remain recordable after revocation, including
resolution of a previously recorded unknown outcome. Replayed outcomes are denied.
Audit contains only normalized attribution and decisions, never command bytes,
output, prompts, environments or secrets.

Evidence: `command_protocol`, `command_authority`, the retained-connection test in
`tests/broker.rs`, `launch_supervisor::command::tests`, and
`launch_supervisor::lifecycle::tool_dispatch::tests`. The last drives the actual
capability gate, seqpacket broker adapter, policy and isolated executor using a
pinned native test process. It covers lost authorization replies, running
revocation/expiry and late actual outcomes. These are deterministic local
composition tests, not a privileged installed-launch or vendor compatibility
claim. Existing identity, descriptor-transfer, isolation and delegation coverage
remains in place.

## Explicit isolated helper grants

The authenticated Agent may send `CommandOperation::Delegate`, containing a
bounded `GrantRequest` and initial `ToolExecutionRequest` for the deterministic
helper. The supervisor never accepts Agent-authored attribution, executable,
mount, network, identity or environment choices. Only this actual Agent channel
can request delegation; a helper channel accepts command requests only.

The fixed release component `louiselm-tool-test-helper` must match its manifest
digest, size and executable designation. The supervisor mounts only that file,
the existing tool workspace/home/system roots, and a new private capability
socket. The Agent runtime, private home and capability socket are absent. The
helper gets independent namespaces and a nested Session cgroup. Existing trusted
startup tracing pins its actual executable and kernel lifetime, and the listener
accepts only that process. Every received packet must also match its kernel sender.
First connection, ancestry, same UID and passed descriptors confer no authority.

The supervisor forwards the original Agent request with its own Agent and tool
attribution over the retained broker connection. `CommandAuthority` checks the
existing operator approval's `allow_delegation`, exact command digest, timeout,
revision, lifetime and remaining aggregate budget. Reservation precedes durable
audit and is never refunded. The broker returns `Granted` only after that audit;
the supervisor independently checks correlation, both lifetimes and the deadline
before installing the grant. Lost replies cannot reconstruct or replay authority.
Unconfigured command policy and missing measured helper support fail closed.

The measured helper forwards its initial work through its own socket, up to its
reserved use count. Every invocation still takes the same broker single-use
decision and supervisor `CommandPermit` as an Agent command. Tool-local sequences
and the Session dispatch sequence remain distinct. The local permit retains both
the Agent and helper lifetime checks, exact immutable command and grant deadline.
This deterministic integration admits one helper lifetime per Session; replacement
requires a fresh launch. It is not a general helper or vendor plugin API.

`BrokerSession::revoke_tool_grant` closes just the named grant's approvals, then
requests enforcement. The supervisor blocks queued starts, closes that helper
channel, terminates its tree, and cancels a running command only when it belongs
to the revoked grant. Other Agent authority remains usable. The broker marks
`tool_grant_revocation_complete` only after confirmed enforcement is durably
audited. Failed cleanup quarantines the Session and poisons its identity lease.

Expiry enforces the same boundary locally, independently of broker notification.
The helper worker observes its deadline and Agent lifetime; command permits
observe both process lifetimes and the earlier command/grant deadline throughout
execution. Whole-Agent revocation and disposal invalidate every grant. Late
authenticated actual outcomes remain recordable; unknown outcomes never refund
or replay a use. Commands and output stay out of normalized audit.

The host suite covers policy, reservation, scope, replay, per-grant revocation,
expiry and queued permits. The disposable-VM test
`privileged_measured_helper_grant_execution_and_revocation` drives the measured
Agent, actual capability and broker seqpacket channels, isolated helper and
command executor. It covers transferred-descriptor denial, queued/running
revocation, expiry, Agent exit, lost grant replies, sticky cleanup failure and
late actual results; Agent authority remains usable after a tool-only revoke.
CI requires it with `LOUISELM_REQUIRE_TOOL_GRANTS=1`; an ordinary
host run explicitly skips its privileged portion. Use the [VM procedure](launcher-vm.md)
with `cargo build --bins` and the built library test under `debug/deps`, then:

```sh
LOUISELM_REQUIRE_TOOL_GRANTS=1 <library-test-binary> \
  launch_supervisor::lifecycle::tool_dispatch::tests::grant_tests::privileged_measured_helper_grant_execution_and_revocation \
  --exact --nocapture
```

## Retained local policy characterization

`louiselm-qbr.5.1.1.4` supplies the local delegation component in
`skills-core/src/broker/delegation/`. The APIs below remain characterization
coverage for helper policy alongside the cross-process production path.
Their in-process commit closure and start-only expiry are not the production
Agent-command contract above.

### Authority and scope

The launch owner supplies an already approved policy, the immutable Session,
Run and envelope binding, and the Agent's authenticated kernel process and
channel. A tool receives authority only when that Agent requests delegation
and the existing operator authorization explicitly permits it. A subset alone
does not imply permission. Automatic delegation within existing approval needs
no additional prompt; this introduces no mandatory approval UI or Agent setting.

The bounded effect is the existing `ToolExecutionRequest`: an exact shell-input
digest, maximum timeout and invocation budget. This is exact command matching,
not an interpretation or safety assessment of shell syntax. Workspace content
can affect a command's behavior; existing confinement and the trusted operator
policy still bound those effects. There is no new executable, mount, identity,
environment, network or credential API.

Each grant reserves part of the Agent's total budget. Failed, abandoned and
completed admissions spend their reservations; dropping or cloning handles does
not refund or multiply authority. Each principal's execution sequence and the
Agent's grant sequence start at one and reject repeated/skipped values. The
integration must assign the supervisor's separate Session-wide execution
sequence when dispatching accepted effects, never forward a tool-local counter
as that canonical counter.

Only supervisor-authenticated, isolated tool process/channel pairs may be
supplied as targets. `BoundProcess` is trusted in-process input, not a wire
record or permission to manufacture a pin from a claimed PID. The component
checks the actual connected peer and every packet's kernel sender against the
pinned target. Helpers, descendants, passed descriptors and replacement
processes gain no authority by membership or ancestry. Agent-originated requests
remain Agent actions under the Agent's scope and remaining budget, including
requests influenced by tool output.

### Asynchronous effects and revocation

`ToolEffect::execute` admits asynchronous preparation and owns exactly one
completion. `PendingEffect::commit` checks identity, scope, subject, revision,
monotonic expiry and parent lifetime again after durable commit intent, directly
around the short irreversible action. Its closure starts that action; it must
not queue it, wait for the whole operation or reenter the authority owner.
Preparation may be arbitrarily delayed without retaining unchecked authority.

Commit returns the started operation and a `CommittedEffect`. The adapter owns
operation completion and cleanup, then calls `finish` with the actual result.
Revocation closes the Agent and all delegated channels before cleanup and denies
pending work. It does not erase effects that already started. Late completion
records and returns their actual outcome. A failure to audit completion preserves
that outcome in `CompletionAudit`; it is not permission to retry or a rollback.

The launch owner calls `revoke` from the existing Agent-exit, revision-change,
disposal and policy-revocation paths. Every admission and commit independently
checks the pidfd-backed lifetime, so late lifecycle event delivery cannot permit
an effect. Drop also revokes. This owner never resumes or transfers authority;
new authority requires a fresh authorized owner. Grants are not recovered after
broker restart. Later lifecycle recovery remains `louiselm-qbr.5.1.2`.

Audit records contain normalized subject, authorization, slot, grant, revision,
process, budget and outcome fields. Commands, output, environments, prompts and
secrets are excluded. Commit intent records uncertainty until an outcome is
recorded. Audit failure denies new effects and closes authority.

### Evidence and limits

`broker::delegation_tests` exercises real credential-carrying Unix packets and
kernel lifetime pins, explicit approval, scope/expiry/revision/replay denial,
budget reservation, queued work after Agent/tool death or owner disposal, and
completion after revocation. Deterministic effect doubles retain the production
asynchronous signature and commit on a worker.

These tests do not prove privileged tool creation or production routing. They
consume the separately tested [process identity](agent-process-identity.md) and
[tool isolation](tool-isolation.md) boundaries. The real integration must obtain
those proofs from the supervisor and connect all revocation events before it can
claim a working production capability path.
