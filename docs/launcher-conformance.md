# Hostile launcher conformance

`scripts/launcher-conformance --disposable-guest` is the required gate for
`louiselm-d6fv.4.8`. Run it only through the disposable VM or in disposable CI,
never with desktop sudo. CI invokes the same recipe.

```sh
git archive HEAD skills-core scripts/launcher-conformance \
  tests/fixtures/verified_posture_v1.json tests/fixtures/provider_config.json |
  ./scripts/launcher-vm exec tar -x -C /home/vm
./scripts/launcher-vm exec env \
  PATH=/home/vm/.cargo/bin:/usr/bin:/bin \
  CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR=/var/tmp/louiselm-skills-target \
  /home/vm/scripts/launcher-conformance --disposable-guest
```

Select uncommitted paths explicitly when transferring work under test. The gate
builds as the guest operator, then runs root checks with private fixture modes.
It includes the sandbox `host_identity` tests under initial-namespace root,
UID/GID 60000 and umask 077, with backtraces enabled. CI consumes this same
recipe. Ordinary Cargo runs skip these privileged cases; the unprivileged
executable-identity regression additionally checks that their shared runtime
fixture matches the kernel executable, including when it uses an interpreter.
UID/GID 60000 and 60001 are the simultaneous Sessions; 60002 is the broker
process double. Allocated accounts/groups, live processes using those UIDs,
missing root, mapped user namespaces, missing tools or kernel controls fail.
The ordinary Cargo suite explicitly ignores the privileged matrix; only the
required recipe is conformance evidence. Test-name inventory prevents an empty
filter from silently passing.

The network fixture is a throwaway **network namespace**, not a user namespace.
It has only loopback, with local test addresses `198.18.0.1` and `fd00:10::1`:
no external interface, default route, bridge or egress. The Agent gets a second,
empty network namespace through production Bubblewrap. This proves inner
network denial independently of QEMU's restricted network. No host or guest
global network/security setting is changed.

## Observations

The same deterministic Rust test Agent issues each probe outside and inside
the production `HostIdentity` backend. Results distinguish allowed operations,
specific kernel denials and unexpected errors; timeouts, absent controls,
duplicate/contradictory results and empty reports cannot pass. Each request has
a deadline, fixtures own process/service cleanup, and the recipe has an outer
deadline. There are 45 named attack checks plus a hostile lifecycle scenario:

| Boundary | Checks | Positive control and denial |
| --- | ---: | --- |
| Outside files, native config/MCP sentinels and symlinks | 10 | Same UID reads harmless targets outside; confined reads fail. No real Provider parser or credentials. |
| Runtime/config writes | 3 | Same UID writes deliberately writable fixture files outside; confined writes return `EROFS`. |
| Ambient environment | 1 | Seeded value visible outside, absent inside. |
| Inherited file/socket FDs | 2 | Deliberately non-CLOEXEC authority works outside; bootstrap refuses before Agent execution; same clean plan then launches. |
| Operator/launcher/broker/second-Session processes; reciprocal Session files/processes | 17 | Live proc reads, actual `strace` ptrace attachment and `SIGCONT` delivery succeed outside; confined attempts fail; targets still answer afterward. |
| Pathname/abstract sockets, D-Bus/systemd and Docker-shaped execution services | 4 | Fixed requests execute a harmless command under the fixture service identity; confined attempts cannot connect or increment execution counts. |
| IPv4/IPv6 unicast-address and loopback TCP | 4 | Real request/reply exchanges before and after confined connection denials. |
| Reciprocal Session channels, private and outer paths | 4 | Declared owner channel and outside control work; other Session cannot connect. |

The forking workload proves a live descendant heartbeat, whole-tree Park,
Resume, foreground work stopping after Interrupt, and zero-survivor Disposal.
A shell trap acknowledgement is not required: namespace supervision can die
before the shell executes that trap. The recipe additionally consumes the
existing `ln30` registry suite and three-scenario production relay/loss test;
neither is reimplemented here.

The matrix consumes the shared
[`conformance` report contract](../skills-core/src/conformance.rs). Successful
completion prints one canonical `HOSTILE_REPORT:` JSON record, after explicit
process cleanup and removal of owned fixture files. Its fixed inventory covers
all 45 attacks plus lifecycle; omitting an entire probe group cannot silently
pass. Missing controls, unexpected errors or interruption yield incomplete
evidence; observed forbidden access or unconfirmed cleanup yields failure.
Duplicate, unexpected and malformed observations are rejected. A failed run
may stop before printing a report, so absence is never success.

The report is explicitly scoped to `disposable_guest`. It has no installed-host
identity, boot measurement, authentication or admission authority. The separate
registry, startup and relay gates remain mandatory. The pure failure-history
reducer retains known failures across incomplete runs, keeps guest and installed
scopes separate, and never clears unresolved cleanup merely because a fresh
fixture was disposed successfully. Protected persistence and applicable-host
validation are separate installed-authority work.

## Installed certification: Debian 13 x86_64

The fixed `louiselm-launch certify` maintenance command runs the shared probe
implementation under the installed launcher authority. It is explicit: ordinary
Session launch never starts it. After installing the corresponding release and
refreshing its launcher authority, the maintainer can invoke:

```sh
sudo -n /usr/local/lib/louiselm/current/bin/louiselm-launch certify
```

The sudo fragment permits exactly `run` and `certify`, both at the same measured
path/digest. It grants neither internal worker verbs nor extra arguments.
Certification checks the installed operator and running release before starting
a fixed worker in a new, empty network namespace. The worker receives its
authorizing parent's identity over an owned pipe and pins its lifetime with a
pidfd. Owner loss or deadline expiry prevents a passing completion. The outer
operation has a three-minute deadline; uncertain cleanup remains uncertain.

The worker leases three available identities from the existing pool, avoiding
occupied or poisoned slots. All files, target processes, socket services and
network endpoints are test-owned. It changes only its own network namespace;
there are no desktop-service requests, real credentials, host-wide settings,
caller-selected commands or new privileged daemon. The complete 46-check
inventory and production Bubblewrap/lifecycle mechanics are required. The guest
gate now imports the same probe implementation; service doubles exchange a fixed
harmless sentinel, not real D-Bus/Docker protocol messages.

### Measured boundary

The supported profile is `debian13-x86_64-glibc/1`. Unknown architectures, OS
profiles, loader layouts, required libraries or unreadable required inputs
refuse certification; there is no force/skip option. The exact profile lives in
[`measurement.rs`](../skills-core/src/conformance/installed/measurement.rs):

- Hashed machine identity, actual boot UUID, release identity, launcher
  configuration and enforced isolation-contract bytes.
- Root-protected launcher/probe, Bubblewrap, glibc loader, `getent`, `ssh-keygen`,
  `strace`, `ip`, `bash`, `sleep` and `unshare` executable bytes.
- The fixed allowed glibc dependency set resolved by the measured loader,
  restricted to `/usr/lib/x86_64-linux-gnu`. The loader configuration/cache and
  bounded configuration-directory inventory are bound too. Nonempty preloads
  are refused. Identity NSS supports `files` and the explicitly measured
  `libnss_systemd` backend; other backends refuse this initial profile.
- Running kernel notes, release/version, command line, taint, loaded module
  names, LSM/cgroup-controller state and the enumerated isolation-relevant
  sysctls. Optional inputs bind presence separately from their bytes.

Inputs are measured before and after the probes. A changed input prevents a
passing certificate, including when all individual probes succeeded. These are
snapshot measurements, **not** full running-kernel/module-memory attestation,
continuous drift detection or a guarantee against root/kernel compromise.
Arbitrary Agent libraries and project build dependencies remain outside this
host-containment profile and retain their separate supply/runtime checks.

### Durable outcomes and inspection

Completed attempts emit exact canonical `louiselm.conformance.certificate/1`
bytes. Exit zero means the new observation report passed; an unavailable,
failed, cancelled or incomplete attempt exits nonzero. A report is not itself
permission for Verified launch. Failures and pending attempts must be inspected
independently, even when an older matching certificate exists.

Root-private state lives at `launcher/conformance` under the release prefix:
`state.json` indexes exact host inputs and retains failure history;
`certificate-*.json` and `observations-*.json` retain exact evidence;
`attempt.json` identifies the owned scratch directory, worker and leased slots.
The directory is `0700`; files are `0600`. Files and directory entries are synced
before acknowledgement. Each probe group checkpoints failures before continuing.
Each record and the currentness index are bounded; evidence is not auto-pruned.

`CertificateStore::inspect` reads the protected state and matching certificate
without mutation. Missing/corrupt existing state or evidence is an error, never
empty failure history. Matching non-passing certificates remain available to
admission: filtering them out would turn stale observations into missing evidence
and incorrectly widen a Missing waiver. Reboot, changed inputs, release changes and incomplete
attempts cannot clear a known failure. Only a new complete covering pass clears
ordinary boundary failures; a fresh fixture's successful cleanup cannot clear
an older unresolved cleanup failure. Abrupt interruption leaves a pending
marker, and unproven process cleanup poisons the affected identity leases.
Do not delete these markers or retry uncertain resources: inspect the retained
attempt and establish original-resource cleanup before an explicit repair.

The privileged installed-certification test runs in a disposable network
namespace and exercises real probes, exact retained reports, stale/reboot
refusal, cancellation and lease release. Unsupported CI hosts explicitly test
refusal instead of claiming that this Debian-specific path passed. The existing
guest hostile matrix remains required. Maintainer acceptance of the actual
installed command is still required. Admission/report transport is tracked by
`d6fv.9.1`; monitoring and user-visible Verified cutover remain `.12.4` and
`d6fv.9`, respectively.

The suite found `louiselm-d6fv.4.8.1.1`: Bubblewrap did not close an undeclared
socket descriptor; the confined workload successfully wrote through it. The
fresh single-threaded bootstrap now rejects undeclared descriptors after the
three declared transfers and before READY/exec. No unsafe raw-FD close or new
crate was added. The independent bootstrap regression fails before the fix and
preserves declared stdio/status/gate transfers after it.

### Admission and broker report retention

The admission evaluator consumes protected certificate/failure state and fresh
host measurements. It refuses retained containment failures, including unresolved
cleanup, even when a passing certificate exists. Missing, stale or incomplete
evidence requires an interactive operator waiver for the exact Session and
condition; unattended execution cannot waive it.

Activation belongs only to the protected installed launcher configuration
(`louiselm.launch.config/3`, `conformance: "pre_cutover" | "enforced"`). First
installation defaults to `pre_cutover`; refreshing the installation preserves
the existing choice. Missing, unreadable or invalid policy is an error, never
a fallback to ordinary launch. The policy is itself measured: a certificate for
the pre-cutover configuration cannot certify a subsequently enforced one.
Do not enable enforcement as part of a routine refresh or test on the desktop.

The broker's authenticated `louiselm.launch.authorization/3` carries explicit
attendance and any already-approved waiver. A waiver binds the exact Session,
request digest (including Run, authorization and envelope revision), operator
UID, condition, exclusive expiry and durable waiver-receipt digest. Unattended,
foreign, expired and containment-failure waivers are rejected. These fields are
not accepted in `LaunchRequest`. The operator approval/receipt producer remains
`d6fv.6.3`; this carrier does not create approval or authorize an Agent to waive.
Cold resume does not inherit the source Session's waiver.

With enforcement active, the supervisor measures the current installed host and
reads protected certificate/failure state before signing sequence zero or
starting the Agent. Unavailable measurements or unreadable failure history are
non-waivable errors. A refusal disposes the prepared process tree and releases
its identity only after cleanup is proved. Expiry is rechecked through receipt
ACKs and before admitting the running Session; this is not lifetime monitoring.

The signed launch receipt records `Certified`, `Waived`, or `Unevaluated`.
Digest-bearing decisions bind the exact canonical **observation report** bytes,
not the enclosing certificate. `ReceiptStore::append` takes those bytes alongside
the signed receipt and makes them durable before acknowledging the receipt.
It rejects missing/mismatched bytes, guest reports, malformed or oversized data,
certification claims over incomplete observations, and any waived containment
failure. An unevaluated launch or a waiver without a digest supplies no report;
unsolicited bytes are refused.

The supervisor sends digest-bearing reports on the same credential-pinned
connection as the signed receipt, before waiting for its ACK. Closed
`louiselm.launch.conformance-report-chunk/1` packets bind the exact signed-receipt
digest, total length and contiguous offset. Reports are bounded to 128 KiB,
fragments to 8 KiB of raw bytes, and the receipt-plus-report receive to one
30-second deadline. Missing, truncated, replayed or foreign fragments fail the
transaction. The broker acknowledges only after the existing receipt store has
verified and retained the complete report; disconnect or storage failure cannot
acknowledge partial evidence. This adds no second report store.

Broker state retains one private report at
`receipts/conformance/<Session>.json`, separate from the unchanged signed receipt
chain. An interrupted append may leave a report before its receipt exists;
retry can reuse only identical bytes and never replaces conflicting evidence.
History reads and authenticated restart revalidate the report against the signed
admission. Missing, changed, oversized, symlinked or nonregular evidence refuses
history use; it cannot become a cached verified result. Trusted callers can read
the exact bytes through `ReceiptStore::conformance_report`.

Canonical posture retains a bounded `conformance_report` reference for isolation.
This is admission history, not a fresh measurement: it supplies neither current
host proof nor a successful-check timestamp. Both the evaluator and status schema
reject using this reference alone as primary evidence for a verified dimension.
Raw observations never enter Session status. Currentness, failure monitoring and
waiver validity remain with their existing conformance producers.

Ordinary pre-cutover launch still records `Unevaluated`, as confirmed in
`louiselm-oi5an`, and does not read certification state. The protected enforced
path can now supply report-bound admission through the broker ACK transaction;
that does not enable Verified posture by itself. `louiselm-d6fv.9.1` still needs
the waiver producer (`.6.3`), currentness monitoring (`.12.4`), canonical status
projection (`.12.6`) and actual installed-release acceptance. Component and
disposable-guest tests do not satisfy that installed cutover.

Recorded 2026-09-06: ten consecutive complete guest rounds passed after fixing
the Interrupt fixture oracle. Environment: Debian 13, kernel
`6.12.107+deb13-cloud-amd64`, Bubblewrap 0.12.0, Rust 1.97.1. Each matrix took
about 1.3 seconds, followed by the existing registry and relay checks. Required
invocations reported no skips. This is local evidence, not a hosted CI result.

## Authority limits

Real: bootstrap, Bubblewrap, assigned outer credentials, mount/PID/network
boundaries, cgroups, descriptor refusal, and production relay/loss composition.
Doubles: Agent, operator/launcher/broker target processes, service endpoints,
signer and broker used by the composite relay test. The service requests model
service-mediated execution, not D-Bus/Docker protocol implementation. Provider
native-source packaging and masking remains `louiselm-d6fv.3`.

This gate does **not** issue a runtime attestation or enable Verified posture.
`IsolationEvidence::check` separately rejects incomplete/contradictory dimension
records, but backend mechanism booleans are not a persisted conformance report.
`louiselm-ucj1` records the approved host-conformance policy; its implementation
is tracked by `louiselm-d6fv.12`, blocking the `louiselm-d6fv.9` cutover. Installed
authority still needs `louiselm-d6fv.4.9` and the genuine release ceremony
`louiselm-lm70`.
