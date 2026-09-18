# Grant-expiry fixture reliability

Issue: `louiselm-ia4pw`. Authorized after PR #10 failed on the existing fixture.
Base: `b175dae3e46e8d61244104ebf8ae584de2aa976e`. The final split remains
unapplied; maximum/consumed optimization passes remain 3/2.

## Reproduction

PR run `35337850521`, job `105576738444`, failed the unchanged test at
`tests/grant_authority.rs:376:32`: admission returned `Expired` to `unwrap()`.
The original excerpt is in `final-baseline-grant-failure.log`.

`CommandAuthority::delegate` reserves the budget, records `ToolGranted`
durably, then rechecks the grant lifetime before replying. The fixture gives
this operation 40ms and assumes a successful reply. Audit latency can consume
that lifetime without violating the production contract; the reservation must
still remain spent.

Controlled reproduction used the existing restricted Debian 13 launcher VM,
Rust 1.97.1, 2 vCPU/4 GiB, offline dependencies and
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`. A temporary 100ms delay immediately
after the `ToolGranted` audit, restricted to the fixture's 40ms grant, produced:

```text
thread 'expiry_and_audit_failure_never_reopen_reserved_authority' panicked
at tests/grant_authority.rs:376:32:
called Result::unwrap() on an Err value: Expired
0 passed; 1 failed; 6 filtered out; finished in 0.10s; Cargo exit 101
```

Command, from the guest repository with the environment above:

```sh
bash scripts/test-skills-core --test grant_authority \
  expiry_and_audit_failure_never_reopen_reserved_authority -- --exact --nocapture
```

## Test-only fix

Admission accepts only a `Granted` reply for grant 1 or the specific `Expired`
error. Both must leave exactly one audit entry reserving the expected grant,
process, revision and capped/uncapped budget. After the original 60ms sleep,
the tool must fail specifically with `Expired`; the Agent retains exactly its
unreserved budget. Exhaustion, audit failure and closed-owner assertions now
require their exact errors instead of accepting any failure.

The 40ms grant and 60ms sleep are unchanged. No production behavior, clock,
deadline, public API or dependency changes. Successful grant admission remains
covered by the other integration tests. The fixture no longer depends on which
side of expiry the durable reply occurs.

With the same diagnostic delay, all seven grant integration tests pass in
0.33s, including both capped/uncapped iterations. After removing the diagnostic,
the same seven pass in 0.12s. `command_grants.rs` is byte-identical to the base.

## Acceptance

Format, all-target/all-feature warnings-denied Clippy and Rustdoc pass in the
guest. Browser regressions pass 13/13 using the existing host Node runtime.
The complete local Rust suite passes, including all integration/binary targets
and three Rustdoc tests. Hosted PR run `35345987608` passes all 13 jobs on head
`19290e8f213f34c3cb8cac752d8143913d1a918a`, checked out as test merge
`fd63d5219a29ccb1e7d45eed59ec9583b18b0fc6` against main `b25dfb4`.
Skills job `105602562882` passes the grant target 7/7, the exact expiry
regression and all privileged/installed-boundary gates. Raw acceptance evidence:
`grant-expiry-hosted.json`. Issue `louiselm-ia4pw` is closed with this gate verdict.

Normal exact-head merge was refused because main requires one approving review.
The PR remains open/unmerged with all checks green; `louiselm-e4fzw` tracks the
approval/landing blocker. No required check or approval was bypassed, no policy
changed. Acceptance records stay on a separate branch to preserve the tested
PR head. The disposable VM is stopped (`not-found/inactive/dead`).

This run took 1080s overall in skills-core and 858s in the measured privileged
step on Intel Xeon Platinum 8370C, 4 vCPU and 15988 MiB RAM. This is a third
hardware cohort, distinct from both earlier AMD runners, not a measured code
speedup. Fresh comparable baseline/candidate collection remains pending.

Host and guest SHA-256 match:

- Changed `grant_authority.rs`:
  `e9fff5c53b597a56dcfdd0ced27f1e7577e0d8976230f8924a65e65f2b7b21be`.
- Unchanged `command_grants.rs`:
  `cf7e0254134ead1028bf4a6fb07e4e07d33a3d60bd833928b449623afe6c7882`.

Lesson: expiry tests must distinguish a durably reserved grant from delivery of
a still-live reply. Prove the reservation and exact later refusal, not a narrow
I/O completion deadline. Evidence is routed here and to `louiselm-ia4pw`.
