# Explicit tool delegation

`louiselm-qbr.5.1.1.5.2` implements the Agent-command path described by
`louiselm-cross-process-tool-commit-z288`: broker-owned approval and budget,
supervisor-owned process identity and actual execution. Delegated helper
creation remains `.5.3`; installed integration and user-visible Verified
cutover remain `.5.4`. This component does not enable an installed vendor Agent.

## Cross-process Agent commands

`SystemCapabilityGate` receives a bounded `ToolExecutionRequest` only from its
pinned Agent, checking both connection and per-packet kernel credentials.
The Session owner forwards the original request and supervisor-authored
`CommandPrincipal` through the retained authenticated broker connection.
The broker never receives a kernel handle and never constructs one from a PID.
An ungranted child, passed descriptor, or first connector cannot become the Agent.

The broker worker attaches an existing approved `DelegationPolicy` to its
`BrokerSession` with `enable_commands`. Its trusted binding must match the
consumed launch's Session, Run, revision and assigned identity. `serve_command`
handles the bounded command conversation on that original connection; it must
not run beside another reader of the same channel. No new human prompt or
Agent permission setting is introduced. Unconfigured command policy denies
commands. The installed broker's overall lifecycle composition is not supplied
by this component.

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

## Helper component awaiting `.5.3`

`louiselm-qbr.5.1.1.4` supplies the local delegation component in
`skills-core/src/broker/delegation/`. The APIs below remain characterization
coverage for helper policy until `.5.3` ports them to the cross-process boundary.
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
