# Attention timeout diagnosis — 2026-09-18

Issue: `louiselm-2ar2u`. Session:
`codex/01a0b350-de3e-7020-9ac9-615b56e88bb6`.

Initial diagnosis below made no production, fixture, workflow or timeout change.
The subsequently authorized local diagnostics are recorded at the end. No new
hosted run or PR was part of this diagnosis. The skills optimization campaign
remains stopped; later commit/sync authorization does not establish CI acceptance.

## Result

**The stalled phase is still unknown.** The failed hosted job has no stack or
phase output. Two disposable-VM executions passed; neither reproduces the
hosted timeout. This does not establish a fix or exclude a timing-dependent
production failure.

Fixture-only times from completed GitHub job logs (build time excluded):

| Revision | Job | Result |
| --- | --- | --- |
| `9b29492` | [105636792847](https://github.com/euri10/louiselm/actions/runs/35356424348/job/105636792847) | 38.981s, pass |
| `9b29492` | [105636791673](https://github.com/euri10/louiselm/actions/runs/35356424263/job/105636791673) | 3.914s, pass |
| `9b29492` | [105636792262](https://github.com/euri10/louiselm/actions/runs/35356424098/job/105636792262) | 25.317s, pass |
| `eacdcc8` | [105647926240](https://github.com/euri10/louiselm/actions/runs/35359785856/job/105647926240) | 50.199s, pass |
| `eacdcc8` | [105647936807](https://github.com/euri10/louiselm/actions/runs/35359785796/job/105647936807) | 21.970s, pass |
| `eacdcc8` | [105647936369](https://github.com/euri10/louiselm/actions/runs/35359785939/job/105647936369) | outer 120s timeout, exit 124 |

The baseline and failed job report Ubuntu 24.04 image `20260907.300.1`.
The capture crate, Attention fixture, installer and capture workflow job are
unchanged between the two revisions. Large runtime variation already exists
on the baseline; these observations cannot identify the responsible phase.

## Bounded local probes

Used the existing disposable Debian 13 VM: 2 vCPUs, 4096 MiB configured memory,
restricted networking, no host mount or forwarded credentials, no USB device.
This is **not** an environment-equivalent reproduction of the hosted image.

Built current clean capture sources with
`cargo build --all-features --locked --offline`; copied only the binary into
the guest. The fixture and installer in the existing `9b29492` guest checkout
matched the current files by SHA-256:

```text
binary    aa1b3ac39769f657533be21444f34d8559b34f07c9b8277cfc6238af18f95236
fixture   e2ce423d0af1bd202cecdc3f04584bc5111ec3189a6b69dcbcf8d9b54f6b8448
installer f2afa3b4451f155209cb5a0f5de6843cc89568bf09174cec44d41fa676aed465
```

Both executions kept the original `timeout 120 unshare --mount --propagation
private` boundary and explicit privileged-gate opt-in. Never run this fixture
on the maintainer host.

1. Temporary external Python wrapper used `sys.settrace` to print selected
   fixture line boundaries and monotonic time, plus
   `faulthandler.dump_traceback_later(15, repeat=True)`. It printed no locals,
   configuration, credentials or payloads. The checked-in fixture was unchanged.
   Test passed in **1.084s**; no stack dump was needed.
2. Unmodified invocation without the wrapper passed in **0.887s**. This control
   checks that the successful local result did not require instrumentation.

Selected observed phase starts from the traced execution, seconds since wrapper
startup (these include interpreter/test startup, unlike unittest's duration):

```text
copy-binary       0.047
copy-etc          0.090
mount-etc         0.335
accounts-ready    0.404
start-unset       0.404
stop-unset        0.456
install-first     0.457
install-repeat    0.502
start-provisioned 0.530
unknown-client    0.720
stop-provisioned  0.757
start-replay      0.758
stop-replay       0.892
permission-drift  0.905
cleanup          0.927
userdel          0.928
userdel          1.029
unmount-etc      1.110
```

In this environment `/etc` copying took 0.245s. That is not evidence that the
same operation was fast or slow in the failed hosted execution. All original
unset, cross-identity, capability-refusal, replay and permission-drift assertions
ran. Guest account/group and receiver-process checks were empty; provisioning
paths were absent. The VM was stopped afterwards (`inactive`, `MainPID=0`).

Temporary probe retained at
`/var/tmp/louiselm-attention-diagnosis.R3RNuj/trace-attention.py`; guest copy and
binary at `/home/vm/attention-diagnosis.1C7hX5/`. No dependencies were added.

## Next discriminating evidence

The failed runner is gone; its existing log cannot reveal the missing phase.
Add bounded, payload-free phase markers and a pre-deadline Python stack dump
to a diagnostic invocation on the hosted image. Keep the 120s watchdog and all
assertions. A passing retry alone is not acceptance: capture a slow/stalled
phase, then design its regression before fixing it. Local instrumentation was
subsequently authorized below; a hosted run still needs authorization.

Lesson stays on `louiselm-2ar2u`: timeout exit status establishes a deadline
failure, not which unbounded call caused it. Do not label `/etc` copying,
receiver shutdown, or runner load as the root cause without phase evidence.

## Authorized local diagnostics

Added static phase labels and monotonic elapsed time to the installed fixture's
stderr, flushed before each potentially slow boundary. Its unittest setup owns
one `faulthandler` dump at 90s; unittest cleanup cancels it even on failure.
The dump reports stack locations, not locals or child payloads, and does not
exit the process. Client mode and imported Admission helpers do not arm it.
Every original assertion and the CI `timeout 120` command remain unchanged.

The standard-library helper is covered by
`scripts/test-broker-attention-diagnostics.py`, wired into the existing capture
CI step. No dependency or logging framework was added.

Verification:

- Red: both initial regression cases failed with missing `diagnostics` before
  implementation.
- Green: all three regressions pass on host and guest: immediate flushing with
  line buffering disabled; exactly one synthetic-stall dump without payloads or
  process exit; cancellation after normal and exception exits.
- A separate unprivileged probe using the actual default delay emitted exactly
  one `Timeout (0:01:30)!` stack, then printed `91.001s probe complete` and exited
  zero. This checks the 90s default without slowing the routine regression suite.
- The complete installed Attention gate passes in **0.867s** inside the existing
  disposable Debian VM, with phase output through scratch-directory cleanup and
  `complete`. It reused the SHA-256-verified current-source binary above.
- Offline locked capture build and all 12 release/workflow contract tests pass.
  Attention and Admission privileged gates correctly skip without their opt-in
  on the host; these skips are not privileged acceptance.
- Guest fixture accounts/groups, receiver processes and provisioning paths were
  absent after the run; the VM was stopped.

This is diagnostic instrumentation, **not a timeout fix or a new hosted result**.
`louiselm-2ar2u` stays open for hosted phase evidence and its original acceptance.
