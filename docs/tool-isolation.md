# Zero-authority tool execution

`louiselm-qbr.5.1.1.3` adds one supervisor-owned tool boundary. A command arrives
only on the credential-authenticated Control broker connection. The broker owns
the authorization decision; the Launch supervisor performs confinement. This
component does not authorize arbitrary Agent-originated messages as broker policy.
Real broker capability dispatch remains `louiselm-qbr.5.1.1.5`; explicit nonzero
grants remain `.4`, and installed vendor integrations remain `louiselm-d6fv.9`.

## Exact integration evidence

The optional Agent registration field `tool_integration` must select
`louiselm.test-tool-integration/1`. Missing and unknown selections refuse Verified
launch. The selected release manifest must bind the executable component
`louiselm-tool-test-agent`, and the registered runtime must contain those exact
bytes. Startup arguments and environment must both be empty. Release identity,
executable hash, device/inode, Session and measured Bubblewrap digest are exposed
as `ToolIsolationEvidence::canonical_bytes()` on the authenticated runtime.
The launcher constructs this evidence before startup and checks it against the
kernel-pinned executable before enabling capabilities. A registration name or
an integration's assertion cannot manufacture the evidence.

The deterministic Agent is the executable itself, not a runtime wrapper. Its
entire behavior is opaque stdio echo: it does not load workspace configuration,
plugins or tools, and does not execute commands. The test broker submits tool
commands independently. Support for these exact release bytes says nothing
about another Agent, including an adapter that can execute repository plugins
in its own process. System-library trust and host conformance remain the existing
launcher prerequisites; this is not a new claim of continuous code attestation.

## Command and authority boundary

`louiselm.launch.tool-execution/1` binds a command to Session, Run, envelope
revision and a strictly increasing execution sequence starting at one. Admission
requires a running Session with an enabled capability channel, a connected broker
and no pending lifecycle or tool operation. A sequence is consumed before
mechanics; completed, failed and disconnected executions cannot be replayed.
The broker must reconcile uncertain completion rather than assume a lost response
means the command did not run. Commands and returned output are not logged or
included in receipts.

The closed request has no executable, identity, mount, cwd, environment or grant
fields. Its bounded shell input runs through fixed `/bin/sh -c`, with a fixed
PATH, a separate tool home, the Session workspace and read-only system libraries.
The Agent runtime, private home and capability socket are not mounted. Overlapping
Agent runtime/private paths under the tool's system mounts are refused. Network
access is denied and no input or capability descriptors are inherited.

Each execution gets independent mount, PID, IPC and network namespaces. Its host
UID/GID remains the assigned Session identity, but sharing that identity grants
no authority: namespace separation protects private state, and every capability
packet still has to come from the pinned actual Agent process. Even a descriptor
deliberately passed through a workspace socket cannot impersonate that process.
Workspace writes and untrusted returned output are intentional communication,
not trusted Agent configuration. Agent actions influenced by output remain Agent
actions; no prompt-injection immunity is claimed.

## Lifetime and bounds

One tool executes at a time. A command is limited to 16 KiB and thirty seconds;
each returned stream is capped at 4 KiB of decoded UTF-8. Output flooding cannot
starve deadline or cancellation checks. A tool cgroup is nested under its owning
Session, so Session freeze/kill includes it. Interrupt cancels the current tool
before interrupting the Agent. Disposal cancels and joins the worker, proves the
tool tree empty, then disposes the Agent tree. Cleanup failure stays latched and
prevents identity release. No root daemon, runtime wrapper, replacement Agent or
Bubblewrap reaper change is introduced.

Completion follows tool-tree disposal. The lifecycle owner discards completions
from an ended process/connection epoch. A full event queue cannot block a tool
callback that disposal must join: a bounded mailbox owns that completion.

## Verification

The normal crate suite exercises hostile private-file/process/socket probes,
passed-descriptor and descendant denial with an authenticated positive control,
bounded output, timeout cleanup, cancellation before execution, replay/binding
checks and callbacks after Session disposal. Existing transport tests retain
inherited-descriptor and actual-Agent lifetime coverage.

CI additionally requires the production assigned-UID composition, with no skip:

```sh
cargo build --bins --all-features --locked
# In the disposable launcher VM, run the built library test as root:
LOUISELM_REQUIRE_TOOL_ISOLATION=1 <library-test-binary> \
  launch_supervisor::system::tool_integration_tests::privileged_measured_agent_owns_isolated_tool_lifecycle \
  --exact --nocapture
```

Keep the built library test under `debug/deps` and the two binaries under `debug`.
Use [the disposable VM procedure](launcher-vm.md), never desktop `sudo`.
The privileged test checks the real kernel pin, missing/unsupported registration,
workspace execution, nested-cgroup cleanup and refusal after disposal. Passing it
does not install or approve a vendor integration on the maintainer's machine.
