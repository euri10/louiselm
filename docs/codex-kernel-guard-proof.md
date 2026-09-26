# Stock Codex kernel-guard feasibility

## Production loader and per-Session upstream ownership

`louiselm-qbr.5.1.3.2.4.2`, design confirmed in `louiselm-8a8id`:
the root per-Session Launch supervisor now owns the Rust loader and kernel
enrollment mechanics. This is not production Provider activation, subscription
authentication or installed-host certification. `Brokered` remains refused.
The historical experiments below retain their original scope and results.

The narrow `launch_supervisor::sender_guard` module loads only
`SENDER_GUARD_OBJECT`, attaches all six required programs and protects their
pins in a private, read-only bpffs mount namespace. Eight maps include the new
per-socket upstream storage. `libbpf-rs = 0.25.0` and
`libbpf-sys = 1.5.1` have default features disabled. `cargo tree -e features -i
libbpf-sys` shows no vendored/static features; `readelf -d` on the actual Rust
test executable records `NEEDED: libbpf.so.1`. The host prerequisite was supplied
by the maintainer; no libbpf sources are bundled.

### Enrollment and loss

1. Authenticate and retain the install-authorized, non-root broker channel.
   Loading pins that exact broker and supervisor lifetime, but creates no endpoint.
2. Bind one loopback endpoint, reserve its port **only in its retained network
   namespace**, and leave policy absent. Pre-activation sends fail closed.
3. At the existing measured exec stop, register the exact runtime's pidfd, then
   freeze runtime/broker task grants, owners, loss latch and endpoint reservation.
   Enrollment failure kills the still-stopped workload through the existing
   sandbox cleanup path. It cannot run first and enroll later.
4. Transfer typed `SenderGuardEnrolled` evidence and exactly three descriptors
   over the retained authenticated `SeqpacketChannel`: listener, pin namespace,
   network namespace. The broker validates the signed launch runtime, full
   Session/Run/revision/deadline, original broker PID, namespace types/inodes,
   address and listener cookie. It retains ownership before echoing the exact
   `SenderGuardAccepted` response. Sending alone never permits activation.
5. Activation requires completed enrollment, an acknowledged handoff and the exact
   live scope. A revision must increase; the broker authorizes its new deadline.
   It revokes and shuts down old sockets and requires a new owner response.

After enrollment, the LSM intentionally denies cross-process executable procfs
reads, even from the supervisor. `KernelProcess` therefore retains the frozen
task/loss map handles as its lifetime proof: exact pidfd, unrevoked runtime
grant, live loss latch, and pidfd liveness before/after observation. It never
falls back to procfs or reenrolls. The unguarded identity path is unchanged.
The VM test first reproduced `EACCES` on the old post-enrollment identity check;
the kernel-proof path preserves identity validity and permanently refuses
runtime exec/exit or owner loss.

### Upstream and cleanup contract

The shared unprivileged Control broker chooses the authorized destination and
owns TLS, credentials, request framing and admission. The root supervisor only
connects and registers that exact socket in `BPF_MAP_TYPE_SK_STORAGE` before
authenticated descriptor handoff. No Provider request bytes are sent by the
loader; no BPF descriptor or writable map authority is transferred to the broker.
Each upstream socket belongs permanently to one Session/Run/revision. There is
no cross-Session pool, socket reassignment or recovery enrollment API. Local
map tokens are not truncated Session hashes: each object owns its own full
Rust scope and isolated maps.

The send hook checks the current broker task, socket binding, current endpoint
revision/listener/deadline and kernel loss latch at the actual upstream write.
Supervisor death/exec, runtime exit/exec and broker loss invalidate authority
without a userspace heartbeat. A cached admission cannot bypass it. Other
Sessions and ordinary sockets to the same remote address/port remain usable.
The new namespace-qualified reservation deliberately replaces the old fixture's
host-global port denial: a socket in another namespace is unrelated, while a
sender moved away from its original socket's namespace remains denied. The
updated binding fixture retains that distinction as a regression contract.

Every handoff bundles the socket plus pin-namespace and network-namespace
leases. Accepted connections and partial transfers must retain those leases
until their last usable socket is closed. Pins are never explicitly unlinked
or unmounted during disposal. The supervisor retains upstream socket copies
for shutdown, bounded at 128 concurrent sockets; callers retire completed
connections by exact kernel cookie. Revision change and revoke shut down all
copies, including active streams. No retry or budget refund is implied.
Park revokes this authority too. A warm Resume retains the Session but cannot
reactivate its old revision: the broker must authorize a newer revision and
acknowledge a fresh guard handoff before Provider networking resumes.

`SystemRunningAgent::start_guarded` retains the loader with the existing Session
lifecycle. Disposal revokes before stopping the process tree, closes its own
endpoint after descendant cleanup, and maps uncertainty to `CleanupUnproven` so
the existing identity lease is poisoned. The handoff owner must close its
endpoint/accepted descriptors before completing Session disposal. A broker
restart/replacement requires a fresh Session for
Provider networking; a surviving broker may reattach within existing grace,
without new task enrollment. `.4.5` owns production HTTPS integration and
activation and must preserve these ordering/lifetime requirements.

### Loader gate

`scripts/test-sender-guard-loader.py` runs the production Rust loader only in
the disposable KVM. Two distinct-UID runtimes share one distinct-UID broker
and one synthetic upstream address. The gate checks real authenticated
enrollment, absent-policy denial, frozen maps/protected pins, helper and
cross-Session denials, stale scope and no reenrollment, and kernel identity
checks after enrollment. Eight scenarios cover supervisor death/exec between
admission and write, runtime exit/exec, revision replacement, broker crash,
socket retirement and disposal. Each preserves unaffected-Session and unrelated
traffic controls; all child processes and eight-map inventories must disappear.
No real Account, Provider, TLS secret or installed Agent settings are used.

The `sender-guard-vm` CI job requires this gate and all four existing embedded
ownership variants. The latter retain their missing-hook and explicit detach
negative controls. See `docs/launcher-vm.md` for commands and isolated cache
ownership; never run these loaders with desktop sudo. Unprivileged artifact,
exec-stop cleanup and protocol tests remain in the complete skills-core suite.

### Authenticated socket handoff

`louiselm-qbr.5.1.3.2.4.3` supplies the production handoff boundary.
`bind_session_endpoint` obtains the blocked Session's verified namespace leader,
checks its process-tree membership, and binds on a scoped thread which enters
only that network namespace. Enrollment checks the measured runtime's namespace
against the retained listener. No route, namespace change in the broker, or
credential in Session state is introduced.

The control transport accepts rights only on the two typed guard transfers,
with exactly three close-on-exec descriptors and ordinary per-packet credential
authentication. A `BrokerSession` owns its accepted listener. Its serving method
uses the existing HTTP parser, durable request admission and streaming relay;
accepted sockets stay within that serialized worker and close before it can
acknowledge disposal. Reusing a retired endpoint revision is refused.

`handoff_upstream` creates and guards one already-admitted destination, transfers
the socket on the same channel and waits for an exact acknowledgement. The Rust
receiver checks the stored Provider destination, Session enrollment, connected
peer, cookie and both leases. `GuardedUpstream` carries those leases through
reads/writes and shuts down every duplicate on drop. A retained upstream owner
prevents a closure acknowledgement, even after shutdown. The supervisor still
retires its copy by cookie; failed acknowledgements revoke all socket authority,
close the channel and deny reuse.

A revision-bound `GuardRevoker` can delete policy and shut down retained sockets
without waiting for connect, descriptor delivery or the broker ACK. Registration
and activation hold only its short state lock; a late ACK cannot restore the
revoked revision. The disposable handoff gate races revocation at all three
stages and rejects a stale handle after revision replacement. This is a
component seam: `.4.5` must still connect it to the production lifecycle owner.

Revision/disposal revokes first, then exchanges `SenderGuardClosing` /
`SenderGuardClosed` for the exact enrollment before releasing the endpoint.
No reply means cleanup is unproved unless the original broker's pidfd proves
process death. Losing a channel to a living broker never proves descriptor
closure. Owned sockets drop before the pin namespace, including guard Drop.

`scripts/test-sender-guard-handoff.py` exercises these production boundaries in
the disposable KVM with two runtime UIDs, a separate broker UID, separate empty
network namespaces, offline Provider responses and synthetic signed launch/status
fixtures. It checks successful parser/admission/relay and spent units, inherited
helper and cross-Session socket transfers, guarded upstream handoff, stale writes,
invalid listener cookies, refused upstream acknowledgements, uncertain cleanup,
broker crash and final process/map cleanup. CI requires it alongside the loader
gate. This is component evidence: `.4.5` still owns the
stock runtime configuration, guarded HTTPS transport composition and global
Brokered activation. The installed launch entrypoint still refuses Brokered.

## Historical feasibility evidence

`louiselm-qbr.5.1.3.9`, 2026-09-21: **the complete stock ACP composition
passes in a disposable guest; no installed cutover or Verified certification**.
The maintainer selected stock Codex and authorized a bounded disposable-VM
investigation. No runtime patch, desktop installation, production request
implementation, new package, or weaker authority contract was introduced.

The original eBPF LSM `socket_sendmsg` experiment let the measured Codex
app-server complete a
synthetic streaming turn while denying its actual shell tool access to the same
endpoint. This improves on the routing-only counterexample in
`docs/codex-endpoint-proof.md`. It is not Verified acceptance.

## Exact experiment

- Guest: Debian 13, `6.12.107+deb13-cloud-amd64`, KVM. `CONFIG_BPF_LSM=y`,
  `CONFIG_BPF_SYSCALL=y`, `CONFIG_DEBUG_INFO_BTF=y`; `bpf` was already enabled
  in `/sys/kernel/security/lsm`.
- Codex: unchanged `0.153.4`, SHA-256
  `56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da`.
- Host compilation: existing Debian Clang 19.1.7 BPF target and Linux UAPI
  headers. Guest loading: existing `libbpf.so.1` through Python ctypes.
  No headers or libraries were vendored or installed.
- A fresh overlay of the immutable prepared guest used a private copy of
  `scripts/launcher-vm`, changing only the unit and SSH port. The existing
  shared VM became active concurrently and was neither modified nor stopped.
  Experiment unit: `louiselm-ow3ok-bpf-vm.service`; loopback port: `22555`.
  Existing 2-vCPU/4-GiB guest, 5-GiB unit ceiling, one-hour deadline, restricted
  QEMU networking, pinned SSH identity and no host mounts were retained.
- The root-owned synthetic HTTP endpoint and loader lived inside that guest.
  The runtime and helpers ran as UID/GID 65534 without supplementary groups.
  Codex received an empty temporary home and a minimal environment. There were
  no account credentials, upstream requests or host credential mounts.
- Codex's inner sandbox was disabled for the fixture. Its tool's denial
  therefore demonstrates the external kernel hook, independent of cooperative
  tool restrictions. No installed Agent configuration was changed.

The probe rule matches the destination port and admits only a configured host
TGID before a monotonic deadline. The controller launches the process, opens a
pidfd and, for Codex, checks its executable against the measured binary before
enabling sends. The hook checks the current sender, not the socket creator;
other LSM denials are preserved. This deliberately small rule is **not** a
production identity, endpoint-binding, lifecycle or authorization design.

## Results

| Case | Observed result |
| --- | --- |
| Authorized synthetic process | HTTP requests accepted through `send`, `sendmsg`, `write`, `writev`, `sendfile`, `splice` and `sendmmsg` |
| Forked helper inheriting the already-connected TCP socket | All seven methods denied with `EPERM`; no extra HTTP request reached the endpoint |
| Policy revoked with the socket still open | Next send denied |
| Deadline already expired with the socket still open | Next send denied |
| Fresh authorization after those denials | Send accepted |
| Stock Codex app-server | Streaming tool-call turn completed and returned `OFFLINE_PROBE_OK`; exactly two synthetic Responses requests reached the endpoint |
| Actual Codex shell tool | Its attempted POST denied with `EPERM` |
| Concurrent sibling process | POST denied with `EPERM` |
| Helper after Codex termination, wait and explicit revocation | POST denied with `EPERM`; this is not automatic kernel revocation proof |
| Two same-destination requests in one authorized `sendmmsg` | Both accepted, with **one** LSM send-hook check |
| Explicit detachment of the guard | Inherited-socket helper accepted: the prototype fails open when its enforcement hook disappears |

Every helper denial had a successful authorized positive control. Unexpected
errors and timeouts failed the experiment instead of counting as denial. The
stock run exercised app-server directly; it did not execute the full
LouiseLM → acp-proxy → codex-acp chain.

## What the edge cases mean

A send hook authenticates a sender at a kernel operation boundary, not an HTTP
request boundary. Linux deliberately reuses the LSM decision for repeated
destinations within `sendmmsg`; the observed count matches the
[Linux 6.12 socket implementation](https://github.com/torvalds/linux/blob/v6.12/net/socket.c).
Multiple HTTP requests can also share one ordinary write. The broker must
independently validate lifecycle, revision, expiry, operation, Model and budget
before each upstream request, including requests buffered before revocation.
One hook check must never be treated as one authorized model request.

The [BPF LSM interface](https://docs.kernel.org/bpf/prog_lsm.html) supports
privileged security hooks; its existence does not prove complete confinement.
This probe keeps the attachment in a process-owned link. Detaching it removes
enforcement. Production would need protected attachment/map ownership and a
proven refusal path when the guard is missing or lost, with no window in which
the endpoint continues accepting unauthenticated traffic.

## Primitive-stage proof gaps

This was the gap list after the first primitive. The `.3.6`, `.3.7`, `.3.8`
and `.3.9` sections below preserve the evidence that closes the bounded proof
slices. Production Provider requests, installed-host certification and
subscription authentication remain separate work.

1. Bind the exact measured runtime lifetime and Session/Run/revision to an
   immutable endpoint identity. The prototype's numeric TGID and port are
   insufficient: prove PID reuse, exec/replacement, other network namespaces,
   cross-Session access, address reuse, IPv6 and concurrent policy updates.
2. Prove guard startup, protected attachment lifetime and map access, loader or
   supervisor death, broker loss, recovery and cleanup. Never remove enforcement
   while a reachable endpoint can still accept requests. Root/kernel compromise
   remains outside the existing host threat model.
3. Extend the hostile matrix to descriptor passing, asynchronous I/O and
   `io_uring`, alternate send paths, partial/multiple requests, queued data,
   in-flight expiry and responses arriving after disposal. The probe has no
   `io_uring` result. Fork inheritance is not an SCM_RIGHTS transfer test.
4. Run the complete measured ACP integration and enforce the approved Model
   allowlist, shared durable request budget and expiry through the real broker.
   Include process/tool isolation beyond endpoint writes; this probe does not
   establish protection against process-memory access, descriptor theft or
   untrusted code executing inside the authorized runtime.
5. Pass the independent subscription sign-in/refresh proof
   `louiselm-qbr.5.1.3.4`. No account authentication was exercised here.

The maintainer confirmed this BPF-LSM candidate and its proof gates on
2026-09-18 in `codex/01a0b2b8-3e82-7b02-ab50-9b40601ac7da` ("ok i confirm").
The follow-up tasks are `louiselm-qbr.5.1.3.6` (exact lifetime binding), `.3.7`
(guard survival), `.3.8` (per-request admission) and `.3.9` (complete ACP proof).
The complete-integration section records the later `.3.9` result; production
`.3.2` still retains its own work and independent authentication `.3.4`. The
concurrent unresolved subscription-bridge proposal
is preserved as the `needs-design` question `louiselm-qbr.5.1.3.10`, blocking
`.3.4`. This confirmation does not establish Verified protection or approve
that authentication proposal. Vocabulary delta: No change.

## Reproduction

Source fixtures are `scripts/probes/codex-kernel-guard/guard.bpf.c`,
`guard_probe.py` and `stock_probe.py`. The stock fixture imports the existing
`scripts/probe-codex-endpoint.py` as `endpoint_probe.py` for synthetic SSE only.
Both entry points require root **inside a disposable KVM guest**. Do not run
the loader with desktop sudo or target another session's VM.

Compile the BPF object without loading it on the host:

```sh
clang -target bpf -g -O2 -Wall -Werror \
  -I /usr/include/x86_64-linux-gnu \
  -c scripts/probes/codex-kernel-guard/guard.bpf.c \
  -o /absolute/private/scratch/guard.bpf.o
```

Following `docs/launcher-vm.md`, transfer only the fixture files, compiled
object and exact pinned Codex binary into an independently owned restricted
guest. Place the two Python fixtures at `/var/tmp/guard_probe.py` and
`/var/tmp/stock_probe.py`, the existing SSE fixture at
`/var/tmp/endpoint_probe.py`, and the binary at `/var/tmp/ow3ok-codex`.
The Python files must be readable and the binary executable by UID 65534.
Then execute **inside that guest**:

```sh
sudo -n python3 /var/tmp/guard_probe.py --disposable-vm /home/vm/guard.bpf.o
sudo -n python3 /var/tmp/stock_probe.py --disposable-vm /home/vm/guard.bpf.o
```

The expected verdicts are `PRIMITIVE_ONLY_NOT_VERIFIED` and
`STOCK_CODEX_PRIMITIVE_PASS_NOT_VERIFIED`. Retain the structured output, then
stop that exact guest to dispose all descendants and enforcement state.
These opt-in experiments are separate from deterministic CI, which must not
require a real Agent executable, privileged BPF loading or a VM.

Evidence cache: `/home/lotso/.cache/louiselm-ow3ok-bpf.4UnRVt`. The measured
BPF object SHA-256 was
`81c5511050df5ff67641cc30ceae704b03711dcfa9f14fe18cae2c89bdbf1475`;
debug-path differences can change a rebuilt object's hash. The source fixtures
and this report preserve the experiment if the disposable cache is removed.
After the experiments, the private VM was explicitly stopped; its unit reported
`LoadState=not-found`, `ActiveState=inactive`, `SubState=dead`, `MainPID=0`.

## Complete stock Codex ACP integration

`louiselm-qbr.5.1.3.9`, 2026-09-21: **the bounded disposable integration
passes**. This is an integration result, not installed-host certification,
subscription authentication or a claim about the maintainer's program/account
status.

The real LouiseLM headless `Session` API launched this exact process chain:

```text
Neovim -> acp-proxy -> node codex-acp.js -> stock Codex 0.153.4
```

The fixture used the adapter's advertised `gpt-5.2` Model with a synthetic
Responses endpoint. The gateway carried no headers, tokens or account
credentials. The Session process tree inherited a private network namespace
containing only `lo`, with no ambient route. It inherited a seccomp filter that
returns `EPERM` for `io_uring_setup`, `io_uring_enter` and `io_uring_register`;
stock Codex remained functional under that explicit refusal. The normal proof
entry point never accepts an unsupported asynchronous-I/O path as a pass.

### Measured identities and boundary

| Input | SHA-256 |
| --- | --- |
| stock Codex 0.153.4 | `56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da` |
| Neovim | `cce0a9494c07dad5eef2fc2b10a81ca7bb447c142829c3a4767452fac74228d3` |
| `acp-proxy` | `d1885d617c52169c92a124172bc160be413a11fa5478706f9e6e0dbcceff9e0a` |
| Node | `b2959781cc5a74c357ffa02367efa8a0330cbb1c9cb347732fdfaaaca381cbcd` |
| `codex-acp.js` | `3f2359fe5584c545eb6c0db688cf9805bde415cf531f986875adbb1060749bd8` |
| SQLite CLI used by the adapter | `01e2610becea4b6e85b14e905a05ccfaf418f4b715f3c7641fc2fd5fc9df5da9` |
| binding BPF object | `b95d70ff30f3447fe52f298d049cc1b0d2760e5e8ea88544382502cb72129ea2` |
| lifecycle BPF object | `8af59fef7ca9e9931f627886f6dc2aaa4c13501fd5b306c03e03e938c899255f` |
| integration BPF object | `e20efe51231c2205aad198845f300d3050af0adb893d97c4fbffb94f4809a146` |

The guest was Debian 13 on x86-64 KVM, kernel
`6.12.107+deb13-cloud-amd64`, with BPF LSM and BTF. Exact executable ancestry,
adapter digest and every listed fixture digest are checked before enrollment.
The supervisor freezes the measured app-server through its pidfd before adding
task storage, then publishes the exact loopback listener binding.

The first full-chain run exposed a real same-UID control bypass: a concurrent
helper could attach to the enrolled app-server with `ptrace` and open
`/proc/<pid>/mem`, even though its direct endpoint send was denied. That red
result is recorded in `louiselm-qbr.5.1.3.9.1`. The binding guard now attaches
an `lsm/ptrace_access_check` hook that preserves prior denials, permits
same-process access and rejects every other task targeting an enrolled runtime.
Acquiring lifecycle namespace references was consequently moved before owner
enrollment. Removing that hook was the observed red case; restoring it produces
the pass below. This closes cross-process control in the accepted boundary; it
does not claim resistance to root/kernel compromise.

### Exact composed matrix

| Case | Result |
| --- | --- |
| Full measured ACP chain | Session ready; streaming prompt completed; real tool start/finish events observed; Session disposed |
| Synthetic upstream | Exactly two `POST /v1/responses` requests; neither has `Authorization` |
| Actual stock-Codex tool | `ptrace`, process-memory access, runtime descriptor access and direct endpoint send all denied |
| Concurrent same-UID sibling while the runtime connection is live | The same four operations denied; runtime turn still completes |
| Runtime exit and stale identity | All measured descendants gone; a later same-UID helper has no endpoint authority |
| Exact sender binding | Updated `.3.6` matrix passes all seven send paths, threads, two Sessions, stale revisions, expiry, SCM_RIGHTS, exec, PID reuse, IPv4/IPv6 and namespace crossings |
| Guard/lifecycle ownership | Updated `.3.7` normal, before-check, supervisor-exec/orderly-close and broker-crash variants pass with six protected links |
| Buffered request composition | Ten deterministic `.3.8` cases pass; multiple/partial/queued frames, stale authority, expiry, cancellation and late callbacks never create an unreviewed effect |
| `io_uring` | All three syscalls refused by inherited seccomp; the stock ACP turn remains functional |
| Unsupported host/object/hook | Refused before listener construction; disposable component evidence still cannot certify an installed host |
| ACP logs | 31 real proxy JSONL files retained normal logging; `Bearer ` and `Authorization` are absent |
| Cleanup | Zero measured descendants remain; endpoint closes before the guard is released; lifecycle runs report no owned maps/processes after disposal |

The actual tool runs as untrusted tool code selected through the stock Agent's
normal tool-call path. Tool output can still influence a later Agent decision;
that later request remains an Agent action and spends Agent authority. This is
not prompt-injection immunity, and code executing inside the measured Agent
runtime necessarily has that runtime's authority. The proven separation is
that tools, adapters, helpers and replacement processes cannot acquire or steer
that authority through descriptors, memory access, process control or inherited
sockets.

The integration uses LouiseLM's scoped auto-approval policy only for the named
offline fixture command. It changes no installed Agent configuration,
permission default, ordinary Session behavior or operator-selected YOLO choice.
The existing conformance consumer accepted the new six-link lifecycle artifact
only as incomplete disposable evidence and again refused to certify a host.

### Reproduction, artifacts and limits

Compile the three final objects on the host without loading them:

```sh
clang -target bpf -g -O2 -Wall -Werror \
  -I /usr/include/x86_64-linux-gnu \
  -c scripts/probes/codex-kernel-guard/binding.bpf.c \
  -o /absolute/private/scratch/binding.bpf.o
clang -target bpf -g -O2 -Wall -Werror \
  -I /usr/include/x86_64-linux-gnu \
  -c scripts/probes/codex-kernel-guard/lifecycle.bpf.c \
  -o /absolute/private/scratch/lifecycle.bpf.o
clang -target bpf -g -O2 -Wall -Werror \
  -I /usr/include/x86_64-linux-gnu \
  -c scripts/probes/codex-kernel-guard/integration.bpf.c \
  -o /absolute/private/scratch/integration.bpf.o
```

Following `docs/launcher-vm.md`, use an independently owned restricted guest.
Transfer the exact measured executables above, the Neovim runtime and repository
`lua/` tree. Put `binding_probe.py`, `integration_probe.py`,
`integration_tool.py`, the existing synthetic SSE fixture as
`/var/tmp/endpoint_probe.py`, and `integration_driver.lua` in `/var/tmp`.
Place the repository Lua tree at `/var/tmp/integration-louiselm`, executable
fixtures under `/var/tmp/integration-fixture`, stock Codex at
`/var/tmp/ow3ok-codex`, and the compiled object at the explicit path below.
Then run inside the guest:

```sh
sudo -n python3 /var/tmp/integration_probe.py --disposable-vm \
  /home/vm/integration.bpf.o
```

The only passing verdict is
`STOCK_CODEX_ACP_GUARD_INTEGRATION_PASS_NOT_VERIFIED`. The structured result's
SHA-256 is
`9c19afb7e5d81639dadd71e9bdbcf1aba2271a82ab5e400227cc513e78fe14be`.
Unexpected denial reasons, missing prerequisites, changed ancestry/digests,
ambient interfaces/routes, sensitive log headers, leftover descendants or an
incomplete Session all fail the run.

Evidence cache: `/home/lotso/.cache/louiselm-acp-guard.70DbQS`; independent unit
`louiselm-acp-guard-vm.service`, loopback SSH port 22560. The cache retains the
three measured objects, `integration-result.json`, `ownership-result.json` and
the private overlay; it contains no account credential. After final validation,
the exact VM was stopped and reported `LoadState=not-found`,
`ActiveState=inactive`, `SubState=dead`, `MainPID=0`, disposing its in-guest
processes and loaded guard state while retaining the disk as recoverable
evidence.

Validation: strict Python compilation, Lua formatting, all three BPF objects
with `-Wall -Werror`, the launcher-VM safety contract, the ten deterministic
buffered-request tests, updated primitive/binding/stock/lifetime/ownership KVM
probes, all four ownership variants, the installed-conformance refusal consumer
and the complete integration passed. The request-boundary suite retains one
ignored KVM-only `sendmmsg` driver whose equivalent prior `.3.8` artifact and
updated seven-path binding matrix pass in this same measured kernel family.

This bounded result does not supply production Provider TLS/pooling,
destination policy, durable shared Run accounting, real subscription service
authentication or installed maintainer acceptance. Those remain with `.3.2`,
`.3.4` and the confirmed authentication design. A synthetic pass cannot satisfy
them and does not require repeating program/account verification.
The retained overlay is recoverable evidence, not an installed service.

## Protected-link lifetime investigation

`louiselm-qbr.5.1.3.7`, 2026-09-21: **partial evidence, not lifecycle acceptance**.
`scripts/probes/codex-kernel-guard/lifetime_probe.py` reuses the exact binding
object and fixtures below. The final guard adds runtime-control protection, so
a separate loader now pins all four links in a fresh,
root-owned `0700` bpffs directory. The controller kills that loader with
`SIGKILL` while retaining the synthetic endpoint and assigned-UID runtime.

| Transition | Observed result |
| --- | --- |
| Before loader death | Runtime accepted; inherited-socket helper denied with `EPERM` |
| Loader killed, all its descriptors closed | Pins retain enforcement and referenced maps; helper denied |
| Existing runtime after loader death | Still accepted under its existing grant; pinning does not automatically revoke authority |
| Session attempts to unlink a protected pin | Permission denied |
| All pins removed while controller retains link descriptors | Helper still denied |
| Privileged `BPF_LINK_DETACH`, with a pin and descriptor retained | `EOPNOTSUPP`; helper still denied |
| Send-link pin is its last reference | Helper still denied |
| Last send-link reference removed while endpoint remains reachable | Helper request accepted after deferred kernel destruction: fail-open counterexample |

The explicit detach syscall was initially expected to succeed; the measured
kernel rejected it instead. This agrees with
[Linux 6.12 tracing-link operations](https://github.com/torvalds/linux/blob/v6.12/kernel/bpf/syscall.c#L3086),
which have no detach callback, and the
[syscall refusal](https://github.com/torvalds/linux/blob/v6.12/kernel/bpf/syscall.c#L5058)
when that callback is absent. Generic BPF detach documentation alone does not
establish support for this link type. An immediate denial after final unpinning
also gave misleading reassurance: destruction is deferred. The counterexample
therefore waits at most five seconds for the unauthorized accepted request;
timeouts and unexpected errors fail the probe rather than count as protection.

Pins solve a narrower problem than endpoint admission. This test deliberately
leaves the endpoint reachable to expose guard-reference loss; it does **not**
prove inaccessible-first startup, supervisor/broker-loss composition, concurrent
replacement, queued-request revocation, recovery or host-conformance admission.
It changes no production behavior, Agent configuration or Verified posture.

The maintainer resolved `louiselm-mvatk` on 2026-09-21 in Session
`codex/01a0c1d7-bbcf-7f61-88dd-6b36b225272f`, confirming this boundary:

- Our crashes and accidental privileged cleanup remain in scope. Malicious
  root/kernel compromise remains excluded; our own cleanup bugs are not exempt.
- Preserve the guard while any endpoint or accepted connection remains usable.
  Revoke authority, stop request processing and close connections before
  releasing the guard. Prove supported lifecycle paths cannot drop the final
  reference prematurely.
- Supervisor/broker death must prevent further request admission. A one-time
  loader may exit without revocation only after safe ownership transfer.
- Polling health is not proof of a gap-free boundary. If protected ownership
  cannot establish the invariant, another enforcement boundary is required;
  do not weaken Verified claims to fit the candidate.

This confirms a design requirement, not implementation or lifecycle acceptance.
The ownership continuation below supplies the bounded crash/teardown component;
request-level admission is supplied by `.3.8`; the complete ACP composition is
supplied by `.3.9` above.

Reproduce inside the independently owned restricted guest, with the binding
object compiled as below and `guard_probe.py`, `binding_probe.py` and the new
fixture together in `/var/tmp`:

```sh
sudo -n python3 /var/tmp/lifetime_probe.py --disposable-vm /home/vm/binding.bpf.o
```

Expected verdict: `LIFETIME_COUNTEREXAMPLES_NOT_VERIFIED`, not a security pass.
The fixture reaps its children, closes the endpoint and descriptors, and removes
only its own pins/directory, including on failed assertions. Run it only in a
disposable KVM guest; it deliberately demonstrates an unguarded endpoint.

Evidence cache: `/home/lotso/.cache/louiselm-lifecycle.eD9HkK`; unit
`louiselm-lifecycle-vm.service`, SSH port `22557`, guest kernel
`6.12.107+deb13-cloud-amd64`. This was a private copy of the stopped binding-proof
overlay, retaining restricted networking and the existing resource limits;
neither the shared VM nor the earlier evidence overlay was modified. Binding
object SHA-256:
`1ac3b4321ebfabcd902d7c70356c197b585e8a10dd575ab8a565349c7db6deef`.

Validation: the original primitive, exact-binding, stock-Codex primitive and
stock-Codex binding probes all retained their expected verdicts. A scratch
negative control removing link pinning failed the helper-denial assertion after
loader death; the unmodified lifetime probe passed its counterexample assertions.
Python compilation, BPF compilation with `-Wall -Werror`, the launcher-VM safety
contract and `git diff --check` passed. No production Rust/Lua code changed.
Afterward no owned lifetime pins or fixture processes remained; the private VM
was explicitly stopped and reported `LoadState=not-found`,
`ActiveState=inactive`, `SubState=dead`, `MainPID=0`. Its disk is retained evidence.

## Endpoint-owned guard lifecycle proof

`louiselm-qbr.5.1.3.7`, 2026-09-21: **disposable lifecycle component passes**.
`ownership_probe.py` and `lifecycle.bpf.c` extend the binding experiment. They
implement no installed endpoint, privileged desktop service, Provider
authentication or Verified cutover. The original final-reference-loss
counterexample remains runnable and still demonstrates its failure.

### Protected ownership and admission

The endpoint owner creates a private mount namespace and private bpffs, loads
all six hooks, pins them, then remounts that filesystem read-only. It reserves
the endpoint port before listening and publishes no runtime policy until
explicit enablement. Missing BPF LSM/BTF, an invalid object or a missing required
hook refuses before constructing that listener. The owner later drops to the
distinct broker-fixture UID 4020010 with no effective capabilities and
`no_new_privs`; remounting writable is denied. Runtime UID 4020000 receives no
guard, map, namespace or controller descriptors.

Read-only pins reject even privileged unlink. Frozen reserved-port, runtime
enrollment, owner-enrollment and loss-latch maps reject userspace deletion or
rewriting. Freezing the reserved-port map matters: removing that entry would
otherwise make the original hook treat the endpoint as an unrelated port.
Policy/listener absence denies instead. Kernel hooks can still revoke frozen
task storage and set the loss latch. All six explicit link-detach attempts
return `EOPNOTSUPP` on this measured kernel; unsupported behavior is not a pass.

The fixture then closes **all** loader link/program descriptors while 200
inherited-helper sends run across the transition. Namespace-owned pins retain
the programs and their maps. The endpoint retains only its admission-map
descriptors. There is no health polling that attempts to repair a detached
hook. Normal shutdown disables processing, stops accepting, closes and joins
accepted connections, then closes map descriptors. Crash cleanup relies on
endpoint process death and the retained namespace, not Python `finally`.
The synthetic upstream observer also retains a namespace descriptor until its
own sockets close. No supported cleanup path unpins or unmounts a live guard.

This uses the kernel's [bpffs object references](https://github.com/torvalds/linux/blob/v6.12/kernel/bpf/inode.c)
and [process-exit lifecycle](https://github.com/torvalds/linux/blob/v6.12/kernel/exit.c).
Do not infer that descriptor numbers alone order socket destruction: final
file release can be deferred. The invariant is that no live processing owner
or intentionally transferred endpoint can outlive the namespace reference.
Arbitrary malicious root namespace entry/remount or kernel compromise remains
outside the accepted threat model; ordinary unlink, fd cleanup and our crashes
are tested rather than excluded.

An immutable task-storage entry binds the exact supervisor lifetime to a
single monotonic loss latch shared by this guard's two destinations. Kernel
exit and exec hooks set it; userspace cannot reset it. Enablement additionally
requires an actual supervisor response **after** hook attachment and enrollment,
so a death before enrollment cannot be mistaken for a live authority. This
fixture owns that subprocess and its private pipe. Production must use the
existing authenticated supervisor/receipt channel for that ordering.

The second destination is a synthetic upstream listener. Its distinct kernel
grant admits only the broker owner, not the runtime. Each fixture request checks
current policy, then makes one guarded upstream write. A deterministic barrier
kills or execs the supervisor **after** that userspace check and **before** the
write: the kernel refuses the write, and the observer receives no request.
A cached userspace decision therefore cannot bridge supervisor loss. Writes
already admitted before revocation are not retroactively undone.

Recovery closes the old listener and every accepted connection. It accepts
only the original launch, Session, Run, revision and unexpired deadline, creates
a fresh listener cookie at the same address, and never reenrolls a runtime or
clears owner loss. Old connections remain denied. Replacing a dead/exec'd owner
requires a fresh launch, not recovery of this grant.

### Observed lifecycle matrix

| Case | Result |
| --- | --- |
| Unsupported LSM/BTF, invalid object, missing owner hook | Refused before endpoint construction |
| Listener before enablement | Runtime send denied |
| Enabled runtime; inherited helper | Runtime completes a synthetic request; helper denied |
| Runtime directly addresses synthetic upstream | Denied; only broker has that grant |
| Revocation and exact-authority recovery | Old sockets closed; fresh listener succeeds; changed scope/deadline rejected |
| Privileged unlink; frozen map cleanup/reset | `EROFS`; `EPERM` |
| Explicit detach of each of six links | `EOPNOTSUPP` |
| Unprivileged remount; all loader descriptors closed | Remount refused; 200 concurrent helper attempts denied; runtime positive control passes |
| Supervisor death before request check | Buffered request refused; no upstream effect |
| Supervisor death/exec after check, before upstream write | Kernel denies final write; no upstream effect or grant restoration |
| Broker owner killed while supervisor remains alive | Listener refuses connections; accepted socket reaches EOF |
| Orderly close and crash disposal | Children reaped; listeners/accepted sockets closed; all seven owned map IDs disappear |

Four scratch negative controls fail at the intended assertions: writable pins,
an unfrozen reserved-port map, missing owner revocation, and bypassing the loss
check **only** for the broker's final upstream write. The last mutation permits
the precisely ordered stale-decision request and fails the expected-403 check.
Restoring the guarded implementation passes.

### Existing consumers and remaining integration

The artifact includes an existing `louiselm.conformance.observations/1` report,
with disposable-guest scope and one lifecycle component observation. The opt-in
`conformance::kernel_guard_component_report_cannot_certify_a_host` test consumes
the actual artifact through `Report` and `Certificate`: it is incomplete for
whole-host conformance and cannot become an installed certificate. No parallel
posture, guest-to-host promotion or new Verified claim is introduced.

The existing broker, conformance and launch-supervisor suites passed 269 tests
(five separate opt-in cases skipped), including sequence-0/sequence-1 ordering,
unsupported integration, missing tool isolation, stale conformance, broker loss,
recovery, disposal and ordinary pre-cutover Sessions. These are existing
consumer gates, not proof that the Python fixture has been installed in them.
Stock Codex remains unsupported by the installed deterministic-test integration;
this work changes no Agent permissions or operator-selected auto-approval.

The `.3.9` integration above composes this ownership and final-write boundary
with the complete ACP chain. Production `.3.2` must preserve the
namespace reference for every endpoint/connection owner, freeze enrollment only
after the exact measured runtime and broker are registered, obtain the
post-enrollment authenticated owner response before enablement, bind measured
BPF/host inputs to existing conformance, and enforce loss at the actual upstream
write. The proof uses a fixed synthetic HTTP operation, not production TLS,
connection pooling or Provider destinations. `.3.8` retains per-request framing
and queued-work policy; `.3.2` retains durable shared accounting; `.3.4` retains
subscription authentication. None can substitute this component pass for its
own acceptance.

### Reproduction and validation

Compile `binding.bpf.c` and `lifecycle.bpf.c` with the host-only Clang command
above. Transfer their objects and `ownership_probe.py`, `lifetime_probe.py`,
`binding_probe.py` and `guard_probe.py` into an independently owned restricted
guest. Keep both objects together: the missing-hook negative case deliberately
loads `binding.bpf.o`. Run inside that guest:

```sh
sudo -n python3 /var/tmp/ownership_probe.py --disposable-vm /home/vm/lifecycle.bpf.o
sudo -n python3 /var/tmp/ownership_probe.py --disposable-vm /home/vm/lifecycle.bpf.o --before-check
sudo -n python3 /var/tmp/ownership_probe.py --disposable-vm /home/vm/lifecycle.bpf.o --supervisor-exec --orderly-close
sudo -n python3 /var/tmp/ownership_probe.py --disposable-vm /home/vm/lifecycle.bpf.o --broker-crash
```

Each emits `OWNERSHIP_COMPONENT_PASS_NOT_VERIFIED` only after cleanup checks.
Save one complete JSON artifact, then consume it on the host:

```sh
LOUISELM_TEST_KERNEL_GUARD_REPORT=/absolute/path/ownership-crash.json \
CARGO_PROFILE_DEV_DEBUG=line-tables-only ./scripts/test-skills-core \
  --test conformance kernel_guard_component_report_cannot_certify_a_host -- --ignored --exact
```

Evidence cache: `/home/lotso/.cache/louiselm-guard-owner.J1lPxP`; independent unit
`louiselm-guard-owner-vm.service`, SSH port `22559`, kernel
`6.12.107+deb13-cloud-amd64`. This private copy retained restricted networking,
the existing resource/deadline limits and no host mounts or account credentials.
The original primitive, binding, both stock-Codex probes and lifetime
counterexample retained their verdicts. The full isolated Rust suite passed
1,091 tests (10 opt-in tests ignored); the artifact consumer passed explicitly.
Formatting, strict all-target/all-feature Clippy, warning-free Rustdoc,
13 browser tests, Python/BPF compilation and the launcher-VM safety contract
passed. The measured lifecycle BPF object SHA-256 is
`794c445bc3b68288efdcda539893b202f68682506ded40bd647bfe88ccd39b72`;
debug paths can change rebuilt object hashes.
After the final four variants, the private VM was explicitly stopped and
reported `LoadState=not-found`, `ActiveState=inactive`, `SubState=dead`,
`MainPID=0`. The private disk and structured results remain as evidence.

## Exact lifetime and endpoint binding follow-up

`louiselm-qbr.5.1.3.6`, 2026-09-18: **binding component passes; production
integration and Verified acceptance remain unproven**. The primitive above is
preserved unchanged as historical evidence. New fixtures `binding.bpf.c` and
`binding_probe.py` extend it without changing Rust, the launcher, Codex, or
desktop configuration. `stock_probe.py` accepts the binding guard so both guards
exercise the same real synthetic streaming turn and actual shell-tool denial.

### Binding and ownership

- The privileged fixture freezes the runtime's thread group, holds its pidfd,
  checks the executable, and installs one immutable grant using
  `BPF_MAP_TYPE_TASK_STORAGE` with `BPF_NOEXIST`. Kernel task storage, looked up
  on the current sender's group leader, establishes lifetime; a numeric PID,
  procfs snapshot or possession of a connected descriptor grants nothing.
  Forked helpers do not inherit it. A `bprm_committed_creds` LSM hook permanently
  revokes the grant on exec, including exec of the same binary. Runtime threads
  remain usable; exec by a non-leader thread also loses authority.
- The grant binds a unique launch identity, Session and Run. The endpoint policy
  additionally binds the envelope revision, expiry, exact IPv4/IPv6 address,
  retained network namespace and listener socket cookie. Both sender and socket
  namespaces must match. Namespace descriptors stay open to prevent namespace
  inode reuse. Test identifiers are synthetic integers, not ACP identifiers or
  transferable credentials.
- The broker-side fixture owns one dedicated listener per Session. It derives
  the Session from that listener's configuration, never from peer headers,
  source ports or claimed identity. Two simultaneously authorized Sessions can
  use their respective listeners but cannot send to each other's endpoint.
  The production composition must retain this association: the Control broker
  chooses policy and owns the listener; privileged enrollment belongs to the
  Launch supervisor and uses its existing measured exec stop and
  `launch_transport::KernelProcess` lifetime. This probe exercises the kernel
  mechanics and listener ownership, not that production channel integration.
- Reserved ports fail closed before a binding is published. Each policy update
  replaces one immutable BPF HASH value atomically. One hook invocation reads
  one record; it cannot combine authority fields from different revisions.
  Socket-local storage pins the first permitted send's launch, revision and
  listener. An old connection cannot send under a new revision; a new connection
  can. The trusted broker remains responsible for monotonic revisions and unique
  launch identities. Deliberate fixture rollback supplies negative controls,
  not a production rollback policy.
- `inet_csk_listen_stop` removes the listener cookie from the live set. Closing
  the listener denies further sends on established connections. A new listener
  reusing its exact address and port stays denied until explicitly published
  with its own cookie. This does not prove survival of hook removal or loader
  death: attachment lifetime remains `.3.7`.
- Maps and links belong to the privileged controller; no descriptor is given
  to a runtime or tool. An assigned-UID process cannot acquire the policy map by
  BPF map ID or through the controller's procfs descriptor. Trusted-root/kernel
  compromise and process-memory isolation remain outside this component proof.

The kernel's [task-storage implementation](https://github.com/torvalds/linux/blob/v6.12/kernel/bpf/bpf_task_storage.c)
resolves userspace keys through pidfds. Its
[socket-storage documentation](https://kernel.org/doc/html/latest/bpf/map_sk_storage.html)
describes socket-owned lifetime; neither facility alone establishes authorization.
The proof combines them with the measured stopped process, broker-owned policy,
namespace and listener lifetime checks above.

### Observed matrix

The private KVM guest retained the original kernel and toolchain versions.
Synthetic runtimes used UID/GID 4020000; the second Session used 4020001.
Namespace fixtures used root only to enter/create a namespace, then dropped to
the assigned identity for the denied sends. The stock Codex fixture retained
its original UID/GID 65534 and exact executable digest above. Guest networking
remained restricted, with no upstream requests or real credentials.

| Case | Result |
| --- | --- |
| Reserved endpoint before publication | Send denied |
| All seven original send paths | Authorized sends accepted; inherited helper denied |
| Runtime thread | Accepted |
| Two Sessions and dedicated listeners | Each authorized on its own listener; cross-sends denied |
| Wrong launch, Session or Run | Denied independently |
| Concurrent alternating invalid HASH records | 200 sends denied, zero endpoint effects; valid positive controls pass |
| Revision change | Existing socket denied; fresh socket accepted |
| Expiry | Send denied; renewed positive control accepted |
| SCM_RIGHTS to an independent exec'd process | Recipient denied after authorized sender succeeds |
| Same PID and same executable after exec | Permanently denied |
| Non-leader thread exec | Replacement denied after successful thread send |
| Actual numeric PID reuse, old pidfd still held | New process denied without explicit revocation/re-enrollment |
| Listener close with existing connections | Further send denied |
| Exact address/port reused by new listener | Denied until fresh listener publication |
| IPv6 | Authorized positive control and helper denial pass |
| Same port on another IPv4 address | Denied, no endpoint effects |
| Same address/port in another network namespace | TCP connects, send denied |
| Existing socket after sender changes namespace | Send denied after dropping to assigned UID |
| Tool map access | Map-ID lookup returns EPERM; controller fd access denied |
| Unchanged Codex 0.153.4 | Streaming turn completes; exactly two synthetic Responses requests; shell tool and sibling denied |

An intentional mutation that disables only exec revocation makes the same
fixture fail at the assertion rejecting the post-exec send; the replacement
actually sends successfully. Restoring revocation passes. This is an opt-in
kernel feasibility experiment with behavioral positive/negative controls, not
a production behavior change or a mocked unit-test claim. Unexpected errors,
missing prerequisites, inability to reproduce PID reuse, and timeouts fail.

Complete affected checks: original primitive, original stock Codex fixture,
binding matrix and binding stock Codex fixture; BPF compilation with
`-Wall -Werror`; Python syntax; launcher-VM safety contract. No Rust/Lua code
changed, so their suites do not establish additional evidence for this slice.

### Reproduce the binding proof

Compile `binding.bpf.c` with the same host-only Clang command above, changing
the input/output filenames. In an independently owned disposable guest, put
`binding_probe.py` beside the three original Python fixtures at `/var/tmp/`,
with the same pinned binary and readable compiled object. Run:

```sh
sudo -n python3 /var/tmp/binding_probe.py --disposable-vm /home/vm/binding.bpf.o
sudo -n python3 /var/tmp/binding_probe.py --disposable-vm /home/vm/binding.bpf.o --stock
```

Expected verdicts: `BINDING_COMPONENT_PASS_NOT_VERIFIED` and
`STOCK_CODEX_BINDING_PASS_NOT_VERIFIED`. The PID-reuse case changes only the
disposable guest's `ns_last_pid`; namespace tests create private guest network
namespaces. Do not run these fixtures on the desktop. The exact BTF hooks and
helpers must load successfully; unsupported kernels fail before acceptance.

Evidence cache: `/home/lotso/.cache/louiselm-binding.EvPgbd`; independent unit
`louiselm-binding-vm.service`, loopback SSH port 22556, private overlay of the
prepared guest, original resource/deadline limits. Stop that exact guest after
the runs to dispose its process trees and kernel state.
The completed run was stopped explicitly: `LoadState=not-found`,
`ActiveState=inactive`, `SubState=dead`, `MainPID=0`. The measured binding BPF
object SHA-256 was
`1ac3b4321ebfabcd902d7c70356c197b585e8a10dd575ab8a565349c7db6deef`;
debug paths can change rebuilt object hashes.

### Limits carried into the remaining tasks

This passes `.3.6`'s binding proof, not installed enforcement. `.3.7` supplies
the lifecycle component above; `.3.8` owns
per-request admission, queued bytes, revision races and budget. A send authorized
before policy replacement can already be in flight, and multiple requests can
share that send. The broker must validate each request again before effects.
The `.3.9` section above integrates the full ACP chain, preserves
process-memory isolation, and explicitly refuses `io_uring`. `.3.4` retains
the independent supported subscription-authentication proof. No production
cutover, new dependency, real sign-in or installed Verified claim follows from
these results.

## Buffered request admission proof

`louiselm-qbr.5.1.3.8`, 2026-09-21: **offline request-boundary component passes**.
`skills-core/tests/broker/buffered_requests.rs` composes the existing real broker
launch, authenticated status exchange and `BrokerSession` lifetime with a
bounded HTTP framing fixture and a fake asynchronous upstream. There is no new
production endpoint, authority service, runtime patch or dependency.

The reviewed operation is deliberately small and synthetic: HTTP/1.1
`POST /v1/responses`, exactly one `Host: localhost`, `Content-Type:
application/json`, and decimal `Content-Length`; body
`{"model":"fixture-model","input":"synthetic","stream":true}`. Header names
are case insensitive. Unknown/duplicate headers or JSON fields, duplicate
lengths, transfer encoding, absolute/alternative targets, query parameters,
unreviewed operations, different Models and redirect requests are refused.
The fixture bounds headers and bodies to 4 KiB each and buffered bytes to
16 KiB. It does not claim compatibility with every stock Codex request field,
compression, chunking, HTTP/2 or WebSockets. The complete ACP task `.3.9`
supplies observed stock-runtime requests above; production must review those
fields before broadening the accepted operation.

### Ownership and race boundary

The proof worker owns the actual `BrokerSession` exclusively. Each complete
frame is parsed and validated separately. Its trusted endpoint binding is
compared with the retained launch authorization (including Session, Run,
revision and identity). The fixture checks sender enrollment, current
authority, expiry, the allowed Model and an available reservation before
appending each fake upstream effect. A real authenticated supervisor status
exchange checks current channel/lifecycle state; it is one input to the
decision, never an authorization by itself. Expiry and sender validity are
checked again after that blocking exchange.

Controls and request admissions run on the same exclusive owner, following the
existing broker worker's `&mut BrokerSession` boundary. The fake effect begins
at the ledger append in that owner turn: no cached permit or unchecked work
closure is handed to another executor. Tests order control-before-admission
and admission-before-control from separate threads. Another test invalidates
sender enrollment or advances the clock from the supervisor-reply thread
during status I/O. Already started effects remain spent; later admissions fail.

The sender-enrollment observation, supervisor mechanics, clock and reservation
counter are explicit doubles. This does not implement a second durable Run
budget or prove atomicity of a real Provider write. `.3.2` must substitute its
shared durable reservation transaction and recheck time/identity after storage
I/O, at the actual upstream start. The `.3.9` experiment above composes the per-Session
endpoint binding from `.3.6`, guard/lifecycle ownership from `.3.7`, real ACP
transport disposal and the request-boundary gates. A queued request never gains
authority merely because its bytes arrived before a control change.

### Observed matrix

| Case | Result |
| --- | --- |
| Two requests in one write | Two independent admissions, two spent reservations |
| Every two-way byte split of a valid request | No effect until the body is complete; one admission |
| Queued or partial second request after revocation, lost sender, foreign binding, revision change or expiry | Second request denied; only the first reservation spent |
| Sender loss or expiry during authenticated status I/O | Denied before any fake upstream effect |
| Malformed/unreviewed framing, operation, Model or destination | No upstream effect; connection cannot resynchronize into authority |
| Expiry/disposal after a received output prefix | Local cancellation, prefix retained, late output/completion discarded, spent unit retained |
| Unknown outcome, redirect response or oversized response | No automatic retry, redirect follow or refund |
| Completed response followed by disposal | Completed output retained; completion is not relabelled unknown |
| Independent control/admission producers | Both serialized orderings preserve the effect boundary |
| Actual two-message same-destination `sendmmsg` in KVM | One BPF send-hook check, two Rust broker-fixture admissions, two spent reservations |

Red/green evidence: the initial missing frame/admission implementation failed
the two-request assertion. A later test exposed admission after expiry during
status I/O; the final recheck made it pass. An intentional mutation caching the
first admission allowed the second request after revocation and failed the
queued-request assertion; restoring per-request checks passed.

### Reproduction and limits

The ordinary crate gate runs the deterministic matrix, without an Agent,
Provider, credentials or privileged kernel hooks:

```sh
CARGO_PROFILE_DEV_DEBUG=line-tables-only ./scripts/test-skills-core \
  --test broker buffered_requests -- --nocapture
```

The ignored `buffered_requests::kernel_sendmmsg` test runs only through
`scripts/probes/codex-kernel-guard/request_probe.py` in a disposable KVM guest.
Build the broker integration-test executable with the same Cargo profile,
transfer that exact executable, `request_probe.py`, `guard_probe.py` and the
compiled primitive `guard.bpf.o` into a private restricted guest. Put Python
files in `/var/tmp` with mode 0644 so the assigned-UID synthetic sender can
read them. Run inside the guest:

```sh
sudo -n python3 /var/tmp/request_probe.py --disposable-vm \
  /home/vm/guard.bpf.o /home/vm/request-broker-tests
```

The driver obtains the synthetic frame from the Rust fixture, enrolls its own
UID-65534 sender, and checks both the kernel counter and the Rust admission
result. It closes the endpoint before releasing the experimental guard,
including on failure. Expected verdict:
`REQUEST_BOUNDARY_COMPONENT_PASS_NOT_VERIFIED`.

Evidence cache: `/home/lotso/.cache/louiselm-request-proof.tTNANq`; private unit
`louiselm-request-proof-vm.service`, loopback SSH port 22558, Debian guest kernel
`6.12.107+deb13-cloud-amd64`. The private disk copies retained the wrapper's
restricted networking, resource limits and deadline; no host mounts or desktop
privileged changes were used. Production transport, durable shared-budget
recovery, real subscription authentication and installed ACP/Verified
acceptance remain with `.3.2` and `.3.4`; `.3.9` supplies only the synthetic
composition recorded above.

Validation: 10 deterministic request-proof cases passed; the opt-in request
probe and all four existing primitive/binding/stock probes passed in KVM.
The isolated complete Rust suite passed 1,091 tests (9 opt-in tests ignored
there); formatting, all-target/all-feature strict Clippy, warning-free Rustdoc,
13 browser tests, Python compilation, launcher-VM safety and whitespace checks
passed. Logs and the cached-admission negative control are retained in the
evidence directory. Afterward the private VM was explicitly stopped:
`LoadState=not-found`, `ActiveState=inactive`, `SubState=dead`, `MainPID=0`.
