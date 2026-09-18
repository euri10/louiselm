# skills-core CI implementation campaign

Issues: `louiselm-ljf7w` (job split), `louiselm-edon3` (cache experiment).
The maintainer authorized both after the investigation in `ledger.md`.

Maximum candidate passes: **3**. Consumed: **2**. **Applying corrected split retry.**
All three correctness blockers are closed; all three complete repaired-baseline
hosted runs pass. Pass 2 restores the corrected split, with cache unchanged.
Candidate 1 local repairs: **1 of 2**, preserving the branch-required status.
Candidate 1 moves the measured daemon/cold-resume/failure group to an isolated
matrix VM. Retrying that withdrawn split consumes pass 2; the cache
experiment can use pass 3 only if the remaining stop rules permit it.
No candidate has yet been accepted.

Baseline: `37dd6ba4b78ab802f8e2fdc8923e5daafb62029b`, clean implementation
worktree; skills-core tree `419bb0bb9d40639b133ca1cbe0e870602cc13c01` matches the
archived investigation. The original workspace's unrelated Beads edits remain
there. No Rust implementation or dependency change is in this campaign.

Metrics: skills-core completion wall time, sum of its two runner durations,
setup/cache/build/test timings and queue delay. Hosted samples must use the
complete CI workflow and exact candidate revision, all required gates enabled.
Keep the profile, assertions, timeouts and release-commit coverage unchanged.
Record CPU/OS/RAM from each runner because the earlier logs lacked CPU models.

Candidate 1 acceptance: at least three executions; median wall-time reduction
must exceed both 10% and the earlier 514s observed range, and the new range must
fall below the prior 1104s minimum. Added runner cost target: at most three
minutes versus the prior median, an explicit tradeoff for isolated parallel
work. These conservative thresholds are not a claim of randomized statistical
significance. Cache acceptance thresholds will be fixed from its repeated
baseline before its candidate is applied.

| Pass | Lever | Correctness | Timing | Decision |
| --- | --- | --- | --- | --- |
| 1 | Two matrix VMs, serial `core` / `lifecycle` groups | Local 5/5 + publication 8/8; all 21 guest invocations pass. Repaired hosted runs hit existing fixture defects h157 and 0s97h | Guest 730.100s versus baseline 728.720s; initial hosted probe 894s wall / 1696 runner-seconds | Withdrawn: correctness unresolved; no accepted speedup |
| 2 | Restore corrected matrix split after three defect fixes | Local dispatcher/status 5/5, publication 8/8; exact runner already passed all 21 guest invocations on repaired Rust tree | Pending three complete hosted runs | Applied; no cache change, no gain claimed |

The dispatch regression preserves the 21 original external invocations and their
gate flags, namespaces, masks, filters, timeouts and serial group selectors.
It executes fake Cargo/sudo commands only, tests empty selection and unknown
group refusal, and injects a failing fixture to prove its status propagates.
Both matrix jobs are unconditional; neither continues on error. `fail-fast`
is disabled so the sibling still supplies its gate evidence after a failure.
Release automation's existing failed-overall-CI refusal remains enforced.

Initial hosted probe `35320209429` (`5fb6f6f`) passed every gate, core 802s /
lifecycle 894s; both runners AMD EPYC 7763, 4 vCPU, 15989MiB RAM. The original
immutable 1,243,187,453-byte cache was retained. This is preliminary, not an
accepted result: subsequent read-only branch-protection inspection found
`cargo (skills-core)` required but not its lifecycle sibling (`louiselm-oi9d6`).
The initial split could therefore permit merging a failed lifecycle gate even
though the overall workflow and release guard refused it. Sample 2
`35321462395` was cancelled before completion; its partial timing is excluded.

Repair 1 retains that required context as an always-run aggregate of the
parallel matrix. Its exact shell check passes only `success`; the regression
executes it against success/failure/cancelled/skipped/empty results and checks
both groups are dependencies. Five runner/required-status cases and eight
publication-refusal cases pass. No repository setting or token permission
changed. Three measurements restart on the repaired executable revision;
their wall time and summed runner time include the aggregate job's overhead.
Repaired samples may overlap on distinct temporary benchmark refs at the same
commit, with independently provisioned hosted VMs. This avoids same-ref
concurrency cancellation; record queue delay and each group's actual start.

## Stop and restoration

Repaired executable revision: `60aa9e806c4cd494792bdb01cc0e5d3a9b73a818`.
The first two runs failed; no third repaired sample was dispatched:

- `35322512571`, core job `105527976117`: installed broker fixture reads an
  empty effect file where it expects `authorized`, at
  `skills-core/src/launch_supervisor/installed_tests.rs:823`. This is the exact
  existing create-before-write race in `louiselm-0s97h`.
- `35322622132`, core job `105528326146`: admission lock-release fixture gets
  `Trust::Busy` at `skills-core/tests/admission.rs:645`, matching `louiselm-h157`
  (also previously reported in `louiselm-18m2`). The inherited-descriptor
  mechanism remains a hypothesis; this campaign did not prove or fix it.

Both failures are in unchanged Rust code; their exact excerpts are retained in
`broker-effect-failure.log` and `admission-lock-failure.log`. Prior issue records
describe the same failures before this campaign. No test was weakened or rerun
until green. Both optimization tasks now depend on the existing defects.
`repaired-failures.json` retains both completed runs: lifecycle passed all nine
scenarios in each, core failed, and the branch-required `cargo (skills-core)`
aggregate correctly failed in both. Failed partial workloads are excluded from
performance acceptance. The proposal's failure propagation is verified, but its
full-workflow acceptance is not. Claims were released with blockers recorded.

Per the profiling stop rule, the proposed executable changes were withdrawn:
`ci.yml` and `docs/agent-testing.md` match base `37dd6ba` byte-for-byte, and only
the two newly proposed dispatcher scripts were removed. No original test was
removed. Their replacement is the original inline, sequential workflow contract.
The complete corrected proposal is recoverable as `candidate-1.patch` and from
commit `60aa9e8`; `git apply --check` validates it against the restored files.
Evidence and tracker records remain. Main and repository settings were never
changed. The disposable VM is stopped (`not-found/inactive/dead`).

Restoration checks: exact base diff empty for both restored files, publication
fixtures 8/8, instruction-budget gate 11918/12000 bytes, `git diff --check`.
The full-suite limitations above remain; restoration is not a claim they pass.
Retained production optimization: **none**. Original-to-final accepted delta:
**0**. Maximum/consumed remain **3/1** if resumed after the blockers are fixed.

Guest evidence: `split-guest.json`, same Debian VM, Rust/profile/source tree and
serial workload as the investigation. All 20 Rust invocations and the two-test
Python invocation passed, including positive Debian certification. Sum:
730.100s wall / 646.180s user / 21.612s system. No fixture passwd/group entries
for 60000, 4019000 or 4020000, loaded LouiseLM units or matching processes
remained. This checks preservation, not the hosted parallel speedup.

Cache-design diagnostic, not another optimization candidate:
`cache-freshness-probe.json` records three pairs of warm library-test builds
versus the same build after touching only `skills-core/src/lib.rs` in our guest
copy. The file's SHA-256 stayed
`bf27a9fbbde7ccbcedc57fabe9347bf2a4ac21c3add680dc5303a5a17e44284a`.
Warm wall samples: 0.150, 0.145, 0.085s; touched: 8.997, 4.074, 3.348s.
Cargo 1.97.1's fingerprint diagnostics explicitly mark that input stale.
This is a narrow timestamp probe, not a full fresh-checkout benchmark: a
source-key cache hit will not guarantee no compilation, though newer incremental
state may still reduce it. The hosted cache experiment must measure actual work.

## Resumption after correctness fixes

The maintainer authorized both fixture fixes, then the newly observed workspace
capture defect. Commits `82d5484`, `41afe0d` and `4c31b81` contain those separate
fixes. The last fixes a demonstrated tmpfs metadata collision by comparing
second-scan bytes, one file at a time; it is required correctness work, not a
performance candidate. All three complete parallel local suites pass after it.
Full privileged validation and hosted characterization precede any retry.

Fresh baseline protocol: three complete executions of the original sequential
workflow on the same repaired source revision, same profile, original immutable
cache and all original gates. Separate temporary refs avoid cancellation of
same-ref runs; runner VMs are independent. Record queue/start times and hardware.
The sole workflow addition is read-only kernel/CPU/RAM logging, identical to the
already proposed candidate's observation step. No workload, timeout, cache,
permission or required-status change is made for baseline collection.

Before collecting this baseline, the existing collector was extended to accept
one unsplit job as well as the previously handled split/aggregate shapes.
Red: collecting archived unsplit run `35315696761` failed `AssertionError: []`.
Green: that same run yields the archived 1582s wall/runner total; split probe
`35320209429` still yields 894s wall/1696 runner-seconds and all 12+9 scenarios.
The original assertions for split scenario coverage remain intact. This is
measurement-harness validation, not evidence of a new speedup.

Branch protection was reread: `cargo (skills-core)` remains required with strict
up-to-date checks. No repository setting changed. If resumed, retain the corrected
aggregate and the predeclared gain/cost thresholds; compare with both fresh and
archived baselines, never count incomplete/failed workloads as fast samples.

## Pass 2: corrected split retry

Repaired baseline: `a127d9c4a530a87ec15e5cd0bf030fb37a292607`, all 13 jobs pass
in each of runs `35328351435`, `35328353675`, `35328356265`. Raw evidence:
`repaired-baseline.json`. Skills-core durations: 1648, 1536, 1555s; median
**1555s**, range **1536–1648s**, spread **112s**. Privileged group: 1324, 1304,
1323s; median **1323s**, spread **20s**. Queue delays: 40, 39, 34s. All runners
report AMD EPYC 7763, 4 vCPU and 15989 MiB RAM. Each restores the original
1,243,187,453-byte immutable cache, then recompiles the crate. No cold-cache
measurement is claimed by these three warm samples.

Candidate base: `efe1c68702338428417eb50d2c357b4f1b745dd5` (acceptance records
only after the baseline); Rust tree remains
`cb21e42b65614d728f0fb046bba6c2ef04388aeb`. One causal lever: split the same
serial privileged groups across two independent VMs, preserving the branch-
required aggregate. Restore only the four archived proposal files from
`60aa9e8`; do not alter Rust, profiles, caches, deadlines or repository settings.
Rollback is limited to those four files relative to this candidate base.

Retain the conservative original acceptance: at least three complete candidate
runs; median gain above 10% and the historical 514s range (also above the fresh
112s range), candidate range below historical 1104s minimum, added raw runner
time at most three minutes against both archived and fresh medians. Include
aggregate overhead and report rounded minutes/queue delay. Candidate local
repairs start at 0 of 2; total campaign passes remain maximum 3, consumed 2.
