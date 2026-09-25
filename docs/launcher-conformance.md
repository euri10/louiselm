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

The sudo fragment permits exactly `run`, `prepare` and `certify`, all at the same measured
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
caller-selected commands or new privileged daemon. The complete 47-check
inventory and production Bubblewrap/lifecycle mechanics are required. The guest
gate now imports the same probe implementation; service doubles exchange a fixed
harmless sentinel, not real D-Bus/Docker protocol messages.

The additional `sender-guard` observation loads the embedded production object
through system libbpf, enrolls owned runtime/broker processes, and checks an
allowed runtime send, pre-activation and foreign-sender denials, an allowed
upstream write, and kernel denial of that same upstream after runtime loss.
Cleanup closes every peer/socket and verifies that the owned BPF map IDs are
gone after the private pin namespace exits. The earlier 46-check guest matrix
still checks its exact inventory, but its report is incomplete for this expanded
host contract. Guest reports cannot become installed certificates.

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
- Required kernel BTF bytes and the active BPF LSM list. The measured launcher
  binds its embedded guard; the actual resolved `libbpf.so.1` bytes are required.

Absent, failed or changed Sender guard evidence returns `guard_unavailable`
through conformance admission. No interactive or unattended waiver covers it;
restore support and recertify. Other incomplete conformance observations remain
waivable only when the same current host report proves the guard and cleanup.
Ordinary pre-cutover Sessions remain unevaluated and do not acquire a Verified
claim or a new permission prompt.

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
not accepted in `LaunchRequest`. The authenticated operator flow described in
[broker lifecycle](https://github.com/euri10/louiselm/blob/main/docs/broker-lifecycle.md#interactive-live-conformance-waivers)
produces durable decisions for admitted Sessions and pending interactive launches.
For initial admission, the fixed `prepare` command measures and retains a bounded
root-owned observation without starting an Agent or consuming launch authority.
The broker binds operator approval to that exact request, policy, boot and condition;
the supervisor rechecks them and actual host evidence at launch. Preparation grants
no authority by itself and is usable for five minutes. Existing waivers on running
Sessions retain their original approved expiry.
Cold resume does not inherit the source Session's waiver.

With enforcement active, the supervisor measures the current installed host and
reads protected certificate/failure state before signing sequence zero or
starting the Agent. Unavailable measurements or unreadable failure history are
non-waivable errors. A refusal disposes the prepared process tree and releases
its identity only after cleanup is proved. Expiry is rechecked through receipt
ACKs and before admitting the running Session. The same owner then monitors the
admitted Session as described below.

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

The installed operator can retrieve this evidence with
`louiselm-control session conformance SESSION_ID --json`. The existing
credential-authenticated endpoint returns the exact observation bytes as
`report`, immutable `admission`, historical `waiver` condition/expiry/receipt,
and the latest retained `last_check` with its actual condition and suspension
flag. Absent evidence is `null`, never a fabricated empty report. Bound but
unreadable evidence refuses inspection. This historical read also works without
a live supervisor; use `session inspect` separately for current process state,
posture and permitted recovery actions. Neither command renews conformance.

Canonical posture retains a bounded `conformance_report` reference for isolation.
This is admission history, not a fresh measurement: it supplies neither current
host proof nor a successful-check timestamp. Both the evaluator and status schema
reject using this reference alone as primary evidence for a verified dimension.
Raw observations never enter Session status. Currentness, failure monitoring and
waiver validity remain with their existing conformance producers.

### Current checks, suspension and recovery

For an enforced admission, the existing Launch supervisor starts one validity
check per second. It remeasures the pinned release, dependencies and governing
policy, then inspects protected certificate and failure history. Installing a new
default release alone does not invalidate an unchanged protected pinned release.
Removing/changing that release, changing policy or dependencies, or revoking its
signing authority does. Pre-cutover Sessions do not start this monitor.

Checks have one owned worker; stalled I/O cannot occupy the lifecycle dispatcher.
A mismatch suspends immediately, and five seconds without successful validation
suspends independently of check completion or status requests. This is bounded
deadline handling under ordinary scheduling, not a hard-real-time guarantee.
The supervisor first revokes capabilities, then confirms whole-tree freeze.
If either cannot be proved, it attempts full termination. Unproved cleanup
poisons the identity instead of reporting successful disposal or reuse.

Recertification alone never thaws a Session or restores authority. An explicit,
broker-authorized Resume starts a new check after the request; thaw and durable
Resume acknowledgement must succeed before capabilities are enabled. The fresh
check wait has a deadline and can be cancelled by Disposal. Late check results,
signatures and ACKs cannot restore authority after invalidation. Evidence-only
failures permit this recovery; a retained containment failure requires diagnostic
preservation, Disposal and a fresh Session after recertification. An old waiver
cannot hide a containment failure or be renewed by a read or reconnect.

Authenticated `louiselm.launch.conformance-update/2` packets publish bounded,
ordered source facts to the broker's retained evidence owner. One private record
at `receipts/current-conformance/<Session>.json` retains the latest check and the
last verified report reference/time. Exact replays do not renew timestamps;
foreign, backdated or contradictory updates are refused. Missing/corrupt records
cannot establish current posture. Restart retains the source's original expiry.
Canonical isolation posture uses these facts alongside the immutable signed
admission receipt; other dimensions retain their own evidence and freshness.
Neither reading status nor receiving a passing check implicitly resumes work.
Updates carry a mandatory waiver decision revision. The broker ignores checks
from older revisions and refuses unknown future revisions. Authenticated
`louiselm.launch.waiver-change/1` requests replace only the live conformance
decision; original launch authorization and signed admission stay immutable.
The supervisor acknowledges the exact request, rejects older/conflicting
revisions, discards checks started under an earlier decision, and independently
enforces expiry even when a checker stalls. A late waived result cannot Resume
after its decision has expired.

Canonical Session status separately exposes immutable `conformance_admission`
history (`unevaluated`, `certified` with its report digest, or `waived` with its
exact condition and optional report digest). It comes from the authenticated
launch receipt and survives restart without renewing the decision. Historical
certification or waiver never upgrades current isolation posture: absent current
evidence remains `evidence_missing` with a safe collection action and no invented
success timestamp. Both operator and self-scoped Agent responses include this
bounded history by default; raw reports and operator/process identities do not.

Ordinary pre-cutover launch still records `Unevaluated`, as confirmed in
`louiselm-oi5an`, and does not read certification state. The protected enforced
path can now supply report-bound admission through the broker ACK transaction;
that does not enable Verified posture by itself. `louiselm-d6fv.9.1` still needs
initial-launch waiver preparation (`.6.3`) and actual installed-release acceptance.
Currentness monitoring (`.12.4`) supplies source checks; canonical admission-history
projection (`.12.6`) separately preserves the launch decision. Component and disposable-guest
tests do not satisfy that installed cutover.

### Maintained installed acceptance

Record the exact release, host/boot, governing policy, report digest and Session
IDs with the results. Keep reports local: producer observations are operator
evidence, not a sanitized export. Successful fixture runs and this checklist do
not constitute maintainer confirmation or activate the Verified claim.

1. Confirm the reviewed release is installed, the dedicated broker is available,
   and the chosen policy is deliberate. If `/usr/local/lib/louiselm/current/bin`
   is absent, stop: there is no installed workflow to accept. Do not silently
   install, enable enforcement, change upstream Agent approvals, or edit policy
   to make a probe pass. A change from `pre_cutover` to `enforced` requires its
   own certification under the new policy.
2. Run the fixed installed `sudo -n .../louiselm-launch certify` command shown
   above. Verify owned cleanup and a complete passing report for the current
   host/boot and exact measured release. Observe an unrelated operator workload
   throughout. Incomplete/cancelled attempts must never publish a pass or clear
   existing failure. Exercise interruption and negative boundaries only with
   the owned disposable fixtures, not by damaging desktop containment controls.
3. Launch through the installed authorized controller and inspect both records:

   ```sh
   /usr/local/lib/louiselm/current/bin/louiselm-control session inspect SESSION_ID --json
   /usr/local/lib/louiselm/current/bin/louiselm-control session conformance SESSION_ID --json
   ```

   For certified admission, hash the UTF-8 bytes of the decoded `report` string
   without a trailing newline and compare with `admission.report_digest` and
   the sequence-zero receipt. `jq -j '.report'` extracts those bytes only when
   `report` is a string; handle `null` as absence before hashing. Repeat after
   broker restart/reattachment: exact report, admission and waiver stay unchanged,
   and timestamps are not renewed by either read. Check unknown-Session refusal
   and rejection from another UID. All other Verified dimensions must still be
   established independently.
4. With disposable owned Sessions, exercise missing, stale and incomplete
   evidence. Unattended launches refuse; interactive launches require the
   already-approved exact Session/condition waiver. Verify its recorded expiry
   and receipt, degraded isolation, refusal for another Session or condition,
   and non-waivable containment failure. Initial-launch waiver preparation is
   still tracked by `louiselm-d6fv.6.3`; until it is available, record this step
   as blocked rather than constructing an approval record by hand.
5. Use the maintained disposable conformance monitor fixtures to invalidate a
   relevant input and stall a check under load. Inspect the last condition,
   original last-success time and suspension flag alongside actual process state
   and disabled capabilities. Check the one-second sampling/five-second freshness
   rule, confirmed tree freeze, and termination fallback/identity poisoning when
   cleanup cannot be proved. Preserve the failure evidence. These are scheduling
   bounds, not instantaneous or hard-real-time containment guarantees.
6. Recertify and read both records again: the suspended Session must remain
   suspended. Use the installed broker's explicit operator-authorized Resume
   path only for evidence-only failure; verify its fresh check, confirmed thaw,
   durable receipt and capability restoration. For containment failure, preserve
   bounded diagnostic evidence, Dispose, recertify and create a fresh Session.
   If the installed controller cannot issue the authorized operation, record
   recovery as blocked; neither inspection command is an alternative authority.
7. In the disposable release fixture, install a new release without modifying
   the prior pin: unaffected Sessions continue, and the new release needs its
   own certificate. Then invalidate/revoke the old pin and verify suspension.
   Restart/reboot/expiry and incomplete recertification must not erase recorded
   containment failure. Complete covering recertification alone can clear that
   failure; it still cannot resume old processes.

Automated consumers: `tests/broker/conformance_inspection.rs` exercises exact
large reports, absence, restart, redaction and corruption through the real
operator socket; `tests/broker/current_conformance.rs` checks retained failure
causes without rewriting admission. The required activated-daemon fixture
`privileged_activated_daemon_serves_launches_and_restart` also invokes the actual
operator binary under configured and foreign identities, including pre-cutover
absence. Existing installed certification and hostile monitor fixtures own the
privileged probes, suspension and recovery checks. These gates support this
recipe; actual installed host/operator acceptance remains required by
`louiselm-d6fv.12.5` and `louiselm-d6fv.9.1` before any cutover in `.9`.

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
