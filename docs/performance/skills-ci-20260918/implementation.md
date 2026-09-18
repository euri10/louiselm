# skills-core CI implementation campaign

Issues: `louiselm-ljf7w` (job split), `louiselm-edon3` (cache experiment).
The maintainer authorized both after the investigation in `ledger.md`.

Maximum candidate passes: **3**. Consumed: **2**. **Final retry authorized;
baseline collection pending, split still withdrawn.** On 2026-09-18 the maintainer
said "ok do that" after the explicit request for a renewed bounded attempt.
This overrides the prior two-unsuccessful-candidate stop only for pass 3; it
does not reset the count or authorize a cache candidate beyond the cap.
The three authorized correctness fixes are closed; three complete repaired-
baseline hosted runs pass. Pass 2 hit the separate unchanged reconciliation
fixture failure `louiselm-qq1y1`. Both candidates ended without an accepted gain;
the correctness and two-consecutive-no-gain stop rules stopped that attempt.
Candidate 1 local repairs: **1 of 2**, preserving the branch-required status.
Candidate 1 moves the measured daemon/cold-resume/failure group to an isolated
matrix VM. Retrying that withdrawn split consumes pass 2; the cache
experiment was never applied. The unused third pass does not override those
stop rules; this resumption uses the explicit authorization above, not a reset.
No candidate was accepted.

Baseline: `37dd6ba4b78ab802f8e2fdc8923e5daafb62029b`, clean implementation
worktree; skills-core tree `419bb0bb9d40639b133ca1cbe0e870602cc13c01` matches the
archived investigation. The original workspace's unrelated Beads edits remain
there. Neither performance candidate changes Rust or dependencies; separately
authorized correctness fixes establish the repaired baseline recorded below.

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
| 2 | Restore corrected matrix split after three defect fixes | Local dispatcher/status 5/5, publication 8/8; all 21 guest invocations pass. Hosted core hits unchanged reconciliation fixture qq1y1; aggregate refuses failure/cancellation | No complete candidate sample; remaining warm/cold probes cancelled | Withdrawn: unresolved correctness; second consecutive candidate without accepted gain |

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

Cold-cache observation uses two separate probe refs/worktrees at the unsplit
`a127d9c` and split `77533b8` revisions. Each overrides only the existing cache
key prefix with a unique `cold-probe-20260918-` key, verified absent before
dispatch; no restore prefix is added. This conditions an empty-cache input to
the already validated complete workflow, not a source-sensitive cache candidate.
Both retain the exact Rust tree and every gate. Archive misses, full compilation,
save size/time and total runner cost must be recorded; these single cold
observations do not replace the three exact-candidate warm acceptance samples
or establish a statistically significant cold speedup. Probe changes are not
merged into the implementation branch. No shared cache is deleted to force a
miss. Candidate cache optimization remains unapplied.

## Pass 2 stop and final restoration

Candidate: `77533b86b2af9acd7ccea13324faf55216ca0c3e`. Core job
`105555865674` in run `35331251883` fails the complete library suite:
`operator_attestation_never_replays_unknown_writes_or_refunds_the_attempt`
unwraps `TrackerInvocation(... "lost result after mutation")` at
`beads_mutation_control_tests.rs:55:14`. Result: 334 passed, 1 failed, 3 ignored;
Cargo exit 101. The workspace regressions pass in that same run. Exact excerpt:
`reconciliation-failure.log`; separate open defect: `louiselm-qq1y1`.

The Rust tree is identical to the three passing repaired hosted baselines.
The fixture's first call accepts any error, so it does not prove the runner
was invoked before the retry. Its 100ms permission and the real elapsed-time
recheck suggest an earlier expiry, but the first error was not captured:
**root cause is unknown**, and no duplicate real mutation has been established.
No repair or deadline/assertion change was made for this newly discovered bug.

The failed run's remaining work and warm samples `35331254648` / `35331257262`
were cancelled, as were cold probes `35331542815` (unsplit `66bc267`) and
`35331544420` (split `b037978`). All five runs are completed/cancelled;
`pass-2-stopped.json` retains job-level failure/cancellation and other gate
results. Each split aggregate fails on its non-successful dependency, including
the cancelled probes. This verifies refusal, not full positive acceptance.
No partial workload or cold probe is accepted as a timing/cache result.
Probe-only branches remain separate; no shared cache was deleted.

Per the profiling stop rule, only the four proposal paths were restored to
`efe1c68702338428417eb50d2c357b4f1b745dd5`: sequential `ci.yml`, its testing
contract, and removal of the two proposed dispatcher files. The original inline
privileged invocations replace those dispatcher tests; no original gate was
removed. The read-only runner-resource logging remains. The corrected split
is recoverable from `77533b8`, `60aa9e8` and `candidate-1.patch`.

Restoration checks: exact base diff empty for workflow/contract and Rust;
publication fixtures 8/8; instruction-budget gate 11918/12000 bytes;
`git diff --check`. The restored executable source is the same as the three
fully passing repaired baselines, but the separate intermittent fixture defect
remains unresolved; those prior green runs do not negate it. All three verified
correctness fixes, their regression coverage and evidence remain. The disposable
VM remains stopped. Main and repository settings were not changed.

Final budget: maximum **3**, consumed **2**, pass 2 local repairs **0 of 2**.
Final stop: unresolved out-of-scope correctness failure, and two consecutive
candidates without a measurable accepted gain. Retained performance changes:
**none**; original-to-final accepted optimization delta: **0** (no speedup claim
for the separately changed correctness workload). Cache optimization: **not
applied**. Optimization/required-status tasks remain open behind `louiselm-qq1y1`,
with the active claim released. Any further reliability work needs separate
authorization; any future optimization must explicitly revisit this stopped
campaign's bounds rather than silently resetting them.

## Subsequent reliability acceptance (no optimization restart)

The separately authorized reconciliation fixture fix is `84524b4`:
`reconciliation-fixture-fix.md` records controlled red/green evidence and three
complete local suites; `reconciliation-hosted.json` records all 13 hosted CI jobs
passing in run `35335645097`. Issue `louiselm-qq1y1` is resolved. The split remains
withdrawn, cache optimization remains unapplied, and the campaign remains stopped
after two candidates without an accepted measured gain. Budget remains 3/2;
this correctness fix does not automatically authorize a third candidate.

## Authorized final attempt: baseline and integration

The maintainer subsequently authorized landing the tested fixes and a renewed
bounded attempt. First integrate current GitHub main (release fixes only) and
open the reliability PR. Keep its executable source stable while its full PR
gates run. No force merge, required-check bypass or repository-setting change.
The original main worktree and its unrelated changes remain untouched.

Collect three complete sequential baselines on the reconciled repaired source,
recording exact revisions/events, cache scope, resources, queue delay and the
existing privileged-step attribution. PR acceptance is separate from benchmark
samples if the checked-out merge revision differs. Run samples on separate refs
to avoid concurrency cancellation. Cache/profile/gates remain unchanged.

Pass 3 will restore only the four corrected proposal paths already retained
in `60aa9e8`/`77533b8`; rollback targets only those paths against the new baseline.
Keep the existing thresholds: at least three complete split runs, median wall
gain above 10% and the historical 514s range (and any larger new baseline spread),
range below the historical 1104s minimum, and at most 180s added raw runner time
against archived and repaired baselines. Include aggregate overhead, cache-cold
observations and queue/rounded-runner costs. Permit at most two same-lever local
repairs. A new correctness failure, failed gain/cost acceptance or consumed cap
ends the attempt. Caching remains an unapplied, separately bounded follow-up.
