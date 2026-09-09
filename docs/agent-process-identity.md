# Agent process identity

`louiselm-qbr.5.1.1.2` implements the process boundary agreed in `louiselm-9onz`.
It is a launcher component, not installed Verified acceptance. Production still
refuses launch without enforced Agent/tool isolation (`qbr.5.1.1.3`); tool grants,
real broker dispatch and installed integration remain separate work.

## Creation proof

The supervisor attaches Linux `PTRACE_SEIZE` to its own unreaped bootstrap
`Child`, before descriptor handoff. That owned Child cannot have its PID reused.
Kernel fork events identify and stop the namespace reaper, then its initial
workload. An exec stop identifies the workload's executable before any of its
instructions execute. The supervisor checks all real/effective/saved/filesystem
UIDs/GIDs, empty supplementary groups, and the measured executable's device/inode.
It opens a pidfd while the workload is stopped and retains the measured file.

Bubblewrap's reported `child-pid` is checked against the kernel-selected reaper;
it does not select the Agent. Bubblewrap 0.12.0 reads `--block-fd` before forking
the workload, and its PID1 continues doing the original reaping. The launch
monitor, reaper and authenticated Agent are different processes. See
[upstream Bubblewrap](https://github.com/containers/bubblewrap/blob/v0.12.0/bubblewrap.c)
and [Linux ptrace events](https://man7.org/linux/man-pages/man2/ptrace.2.html).

No runtime wrapper, `--as-pid-1`, root daemon or takeover process is introduced.
No process memory or registers are read. The narrow ptrace FFI is necessary
because stdlib and rustix 1.1.4 expose the pidfd/wait/socket operations but not
ptrace controls. Its pre-implementation review is in issue comment 1446.

Unsupported fork/exec shapes fail closed. A shebang script is not the executable
inode the kernel runs: register the actual measured executable with fixed
arguments, or treat that integration as unsupported. Test fixtures use measured
ELFs, not an interpreter whitelist or an exception in production verification.

## Startup and authority

1. Prepare containment with the workload blocked and capability listener disabled.
2. Persist and exactly acknowledge sequence-zero `Launch/Starting`.
3. Release restricted startup and authenticate the actual workload.
4. Require the platform's enforced Agent/tool-isolation proof.
5. Bind Session, Run, channel, envelope revision, assigned identity and kernel
   lifetime; enable the channel.
6. Persist and exactly acknowledge sequence-one `Start/Running`; recheck the
   lifetime before reporting launch success.

`SystemLaunchPlatform` currently returns `ToolIsolationUnproven` at step 4.
Test doubles can provide fake isolation evidence, but `SystemCapabilityGate`
refuses a missing kernel process pin. Neither serialized binding metadata nor
a public PID field can manufacture the production pin.

Socket peers and **every packet** must match the pinned host PID/UID/GID.
The listener never selects the principal from the first connector, Session
membership, ancestry or a self-reported PID. Cgroup checks remain containment
evidence, not permission. Inherited/passed descriptors retain the sender's
actual kernel credentials and cannot give a tool the Agent's identity.

The pidfd makes exit/reuse independent of numeric PID lookup. Executable metadata
reads are bracketed by lifetime checks; observation failure or an observed image
mismatch irreversibly revokes that pin. This is process identity and startup
measurement, not continuous memory/code-integrity attestation. It cannot prevent
an Agent from performing permitted actions on a tool's behalf.

## Exit, races and cleanup

An ended lifetime immediately denies channel traffic, including queued packets
sent before exit and outgoing responses. Resume retains the same pin; a new
process needs fresh launch authorization. Late accept callbacks must also match
the listener's revocation generation.

The relay polls the pin independently of tool-owned stdio. Bubblewrap gets at
most 100 ms to report an exit status after identity loss. Final ACP draining is
also bounded to 100 ms: finite available output is preserved, but a surviving
writer or stalled controller cannot keep the Session alive. The lifecycle owner
receives one process-exit or `AgentIdentityLost` terminal event, closes capability
access before tree cleanup, and records the appropriate terminal receipt. Future
delegation consumes that same authority-ending event.

Startup tracing stays on one thread. Abort kills pinned tracees before the
enclosing owner proves an empty tree. Exit observation uses `WNOWAIT`; consuming
only ptrace stops leaves the direct Child's exit status for its actual owner
(`louiselm-ysrp`). Tracer cleanup is never sufficient evidence to reuse an
identity lease. Existing cgroup cleanup/poisoning rules still apply.

## Verification

Regressions cover real Bubblewrap creation/exec/reaping, wrong executable and
assigned IDs, startup timeout/early exit, inherited descriptors, queued packets
after death, missing isolation, stale callbacks and terminal audit races. The
non-root Bubblewrap fixture tests creation and executable/lifetime proof; it
cannot clear supplementary host groups. The privileged supervisor fixture checks
the full assigned-identity contract, including the actual workload credentials.

On 2026-09-09, the restricted acceptance VM ran the three-scenario privileged
supervisor test, hostile-isolation matrix and root registry tests without skips.
Explicitly transferred host-built binaries were used: its source build lacks
OpenSSL development headers (`louiselm-gg5r`). No desktop privilege, guest package
change, network widening or hardware passthrough was used. These component tests
do not establish a real broker, real Agent/tool separation or installed cutover.
