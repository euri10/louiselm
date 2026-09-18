# Reconciliation fixture reliability

Issue: `louiselm-qq1y1`. The maintainer authorized this separately after the
optimization campaign stopped. Base: `3812972d4d3318d4d14e6ef09d4f68e09509bede`.
No performance candidate is resumed; maximum/consumed passes remain 3/2.

## Failure and controlled reproduction

Hosted run `35331251883`, job `105555865674`, failed at the second `accept`
in `operator_attestation_never_replays_unknown_writes_or_refunds_the_attempt`:
`TrackerInvocation(... "lost result after mutation")`. The retained original
excerpt is `reconciliation-failure.log`. The first call asserted only
`is_err()`, so the log does not identify its error or prove an invocation.

The shared test permission expired at logical time 100. `accept` checks the
initial logical time, then checks logical time plus real preparation/lock-wait
elapsed time before recording intent and invoking the tracker. These tests
exercise durable effects, not expiry, but their success depended on completing
preparation within 99ms. The sibling lost-result test had the same weak assertion.

Reproduction used the existing restricted disposable launcher VM: Debian 13,
kernel `6.12.107+deb13-amd64`, Rust 1.97.1, 2 vCPU, 4 GiB,
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`, offline dependencies. A temporary
`cfg(test)` diagnostic inserted a 150ms delay immediately before the final
permission check, only for request `uncertain` at time 1. The otherwise original
fixture printed its first result, invocation count and intent existence:

```text
qq1y1 first=Err(Expired) calls=0 intent=false
called Result::unwrap() on an Err value:
TrackerInvocation(Custom { kind: Other, error: "lost result after mutation" })
0 passed; 1 failed; finished in 0.15s; Cargo exit 101
```

The retry made the **first**, not a duplicate, invocation. This demonstrates
the mechanism and reproduces the hosted symptom; the historical first error
cannot be recovered and is not claimed as directly observed.

After removing the diagnostic, permanent coverage runs both reconciliation
outcomes at logical starts 1 and 1000 and demands the exact initial runner error
and one invocation. With the old permission it fails deterministically:

```text
unexpected first result: Err(InvalidGrant); calls=0
15 passed; 1 failed; finished in 0.01s; Cargo exit 101
```

The finite-expiry boundary test passes in that red run; it was not weakened
to make the reconciliation scenario pass.

## Test-only fix

The shared non-expiry fixture uses `u64::MAX` explicitly, removing its incidental
wall-clock deadline rather than choosing another short timeout. Tests of expiry
set a finite boundary themselves. No production permission or clock behavior
changes. The existing concurrency fixture no longer needs its separate 60s
override.

Both uncertain-result fixtures require their exact `TrackerInvocation` message
and exactly one invocation before attempting replay. Reconciliation still tests
both operator conclusions, unauthorized control, restart, identical/conflicting
decisions, original receipt bytes, retained `Unknown`, and no reinvocation.
The spent-budget assertion now requires `BeadsBudgetExhausted`, not any error.

An explicit finite-expiry regression permits logical time 99 and refuses 100
and 101 with zero invocations and no intent record. Existing authenticated
broker integration coverage for expired permission remains unchanged.

Focused command from guest repository (with the environment above):

```sh
bash scripts/test-skills-core --lib broker::beads_mutation::tests -- --nocapture
```

After the fix: 16/16 pass in 0.01s. Reapplying the same temporary 150ms diagnostic
also passes 16/16 in 0.31s, including both exact uncertain-result assertions.
The diagnostic and printing were then removed. Production `beads_mutation.rs`
is byte-identical to the base. Only the two test files change executable code.

## Acceptance

Focused red/green and controlled-delay checks are complete. Format, all-target/
all-feature warnings-denied Clippy and Rustdoc pass in the guest. Browser
regressions pass 13/13 on the host (Node v24.16.0); Node is absent in the guest,
so the browser gate was moved to the existing host runtime, not skipped.
Three complete parallel Rust suites pass: each has 336 passing library tests
(3 existing ignored), all integration/binary targets and doc tests. The new
finite-expiry regression adds one library test.

Full hosted CI run `35335645097` passes all 13 jobs on fix commit
`84524b441d39c44b6f4c641baa0d91ad820d66a3`. The original sequential skills-core
job passes its full suite, browser, distinct-identity, conformance, all enabled
privileged and installed-boundary gates. `reconciliation-hosted.json` retains
job/step results and the exact passing regression lines. This completes the
reliability acceptance; no performance claim or optimization resumption follows.

Guest and host source hashes agree. No fixture accounts/groups for 60000,
4019000 or 4020000, or loaded LouiseLM units remained. The VM is stopped
(`not-found/inactive/dead`); 1.9 GiB guest disk space remained, no cleanup needed.

Final source hashes (SHA-256):

- Unchanged production `beads_mutation.rs`:
  `4e89ef296ab583e4065d124353f014b4f044b00578b9a2e55b512b3323f01a84`.
- `beads_mutation_tests.rs`:
  `8215cac66c987f16d5948b7d5eda19d8d34dea6523106866eb1e4328a90a23f8`.
- `beads_mutation_control_tests.rs`:
  `1f8ecfa7e2dcf0cfe2b8d99ff45a496d0f1255a61a998e5a5deafcab3ea07408`.

Lesson routed here and to the issue: an uncertain-effect fixture must prove
the invocation happened and failed in the intended phase before asserting
replay or reconciliation. Keep expiry inputs explicit in expiry tests, not
as incidental scheduling assumptions in unrelated durable-state tests.
