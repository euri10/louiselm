# Codex endpoint authority feasibility

`louiselm-qbr.5.1.3.5`, observed 2026-09-18: **FAIL_CALLER_ISOLATION**.
The existing local Responses routing hook is insufficient by itself to
authenticate the exact Agent runtime. Production request implementation remains
blocked on `louiselm-ow3ok` and the independent authentication proof.

## Reproduce

```sh
python3 scripts/probe-codex-endpoint.py \
  /home/lotso/.codex/packages/standalone/releases/0.153.4-x86_64-unknown-linux-musl/bin/codex
```

The opt-in Linux probe requires Bubblewrap and Python's standard library. It
pins `codex-cli 0.153.4` and executable SHA-256
`56ef98ab4032d317ab26e9b5e5a175650717351edb16ed9cde0cb6d1734d62da`.
Exit zero means the negative counterexample reproduced; it does **not** certify
isolation. Missing prerequisites, a different executable, failed assertions,
and timeouts fail the probe rather than count as successful denial.

The process runs in `bwrap --unshare-all` with an empty home, private procfs,
tmpfs work area, no external routes, no host credential/configuration mounts,
and a cleared environment. System executables/libraries are read-only mounts.
Only synthetic messages reach a fake loopback HTTP endpoint. No upstream
transport exists. Namespace teardown disposes descendants and scratch files.
This is an explicit installed-runtime experiment, separate from deterministic
CI suites that must not depend on real Agent binaries.

## Observations

The app-server receives `initialize`, `thread/start`, and `turn/start`; the fake
model emits a shell call, then a completed text response. The first client TCP
socket inode belongs to the measured app-server process, checked through its
procfs descriptors and executable. This is diagnostic observation, **not** an
authorization mechanism: a procfs snapshot cannot establish per-message sender
identity or prevent descriptor transfer.

| Case | Observed result |
| --- | --- |
| Measured app-server | `POST /v1/responses`, `stream=true`, no Authorization header; streaming turn completed |
| Actual shell tool launched by app-server | Exact synthetic request/header replay accepted, HTTP 200 and completed SSE |
| Concurrent sibling in the same namespace | Replay accepted while app-server remained alive |
| TCP descriptor connected by harness, inherited by helper | Replay accepted from the helper; this is not an app-server descriptor extraction |
| Fresh helper after app-server termination and wait | Replay still accepted |
| Separate confined Session | Not exercised; production namespace/bridge composition does not exist |
| Stale revision, revocation, expiry, replacement, late response after disposal | Not proven for HTTP; no authenticated HTTP-to-broker binding exists to exercise |
| Malformed operations / alternate upstream | No production mediation proven; the fixture has no upstream or forwarding capability |

The tool receives synthetic request bytes from the fixture to demonstrate that
ordinary HTTP metadata is replayable. No private transcript or credential is
read. Codex's inner sandbox is disabled for this experiment, inside the outer
network/filesystem namespace, to test authority independently of its cooperative
tool restrictions. This is neither installed LouiseLM acceptance nor a claim
that tools escape its separate enforced tool sandbox. A future candidate must
prove that actual composed boundary, including endpoint access from helpers.

The fake server deliberately performs no authorization. Therefore these results
reject **using the routing hook alone as authority**; they do not establish that
all possible integrations with stock Codex are impossible.

## Missing boundary

The inspected adapter chain is LouiseLM → `acp-proxy` → `codex-acp` → Codex
app-server. `acp-proxy` forwards ACP; `codex-acp/src/CodexJsonRpcConnection.ts`
spawns app-server and `CodexAcpClient.ts` maps `providers/set` to `base_url`,
wire format, and static headers. The model HTTP caller is app-server, not the
proxy or adapter. This experiment invokes app-server directly; the full ACP
chain was inspected but not executed or changed.

The [official configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
documents the provider URL and header knobs. Those settings do not establish
LouiseLM's required kernel-authenticated process identity.

`skills-core/src/launch_supervisor/system_command.rs` checks both connection
and per-packet credentials against a lifetime-pinned process. Its accepted
protocol operations do not include an HTTP Provider request. The Unix transport
uses `SO_PEERCRED` and `SCM_CREDENTIALS`; a TCP request cannot be passed through
that gate while retaining the original sender evidence. A translating process
would become the observed sender and would need its own trusted, reviewed
means of authenticating the original runtime for every request.

An endpoint outside the empty network namespace is unreachable by ordinary
loopback routing. An endpoint inside it is reachable by the replaying processes
in this experiment. Moving or bridging it therefore needs an explicit authority
design. A URL, shared header, UID, first connection, ancestry, or procfs socket
lookup alone does not meet the existing contract.

Return to `louiselm-ow3ok` to choose a supported process-authenticated transport
and enforced tool separation, or decide that this integration cannot launch
Verified. Any runtime hook or architecture change needs that decision. No
runtime patch, generic proxy, broader networking, or weaker authority is supplied
by this proof. Keep request implementation blocked until the resulting candidate
passes the entire lifecycle and cross-Session matrix above.

## Verification

The complete opt-in probe reproduced all four counterexamples and the successful
streaming turn. The existing `launch_transport` integration suite passed all
20 tests, including denial of inherited Unix descriptors by per-message process
credentials. Those tests validate the existing Unix boundary, not HTTP mediation:

```sh
systemd-run --user --pipe --wait --collect --working-directory="$PWD" \
  --setenv PATH="$PATH" --setenv CARGO_PROFILE_DEV_DEBUG=line-tables-only \
  ./scripts/test-skills-core --test launch_transport
```

Only an experiment and this report were added. No production behavior changed;
there is no red-green implementation fix or claim of full Rust/Lua suite
acceptance. The failed feasibility result is the deliverable.
