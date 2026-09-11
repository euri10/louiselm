# Explicit tool delegation

`louiselm-qbr.5.1.1.4` adds the broker component in
`skills-core/src/broker/delegation/`. Production launch and channel dispatch
remain `louiselm-qbr.5.1.1.5`; this component alone does not enable Verified
Sessions or establish an installed Agent integration.

## Authority and scope

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

## Asynchronous effects and revocation

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

## Evidence and limits

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
