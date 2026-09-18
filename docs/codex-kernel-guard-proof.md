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
