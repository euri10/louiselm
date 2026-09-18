# Stock Codex kernel-guard feasibility

`louiselm-ow3ok`, 2026-09-18: **primitive passes; integration remains unproven**.
The maintainer selected stock Codex and authorized a bounded disposable-VM
investigation. No runtime patch, desktop installation, production request
implementation, new package, or weaker authority contract was introduced.

An eBPF LSM `socket_sendmsg` hook let the measured Codex app-server complete a
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

## Remaining proof before production

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
Production `louiselm-qbr.5.1.3.2` remains blocked on `.3.9` and independent
authentication `.3.4`. The concurrent unresolved subscription-bridge proposal
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
The retained overlay is recoverable evidence, not an installed service.

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

This passes `.3.6`'s binding proof, not installed enforcement. `.3.7` still owns
startup/guard-loss closure and protected attachment lifetime; `.3.8` owns
per-request admission, queued bytes, revision races and budget. A send authorized
before policy replacement can already be in flight, and multiple requests can
share that send. The broker must validate each request again before effects.
`.3.9` must integrate the real supervisor/broker and full ACP chain, preserve
process-memory isolation, and cover asynchronous I/O/`io_uring`. `.3.4` retains
the independent supported subscription-authentication proof. No production
cutover, new dependency, real sign-in or installed Verified claim follows from
these results.
