# skills-core CI implementation campaign

Issues: `louiselm-ljf7w` (job split), `louiselm-edon3` (cache experiment).
The maintainer authorized both after the investigation in `ledger.md`.

Maximum candidate passes: **3**. Consumed: **1**, evaluation in progress.
Candidate 1 local repairs: **1 of 2**, preserving the branch-required status.
Candidate 1 moves the measured daemon/cold-resume/failure group to an isolated
matrix VM. Candidate 2 will evaluate source-sensitive cache reuse separately.
No candidate is accepted merely because it passes correctness gates.

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
| 1 | Two matrix VMs, serial `core` / `lifecycle` groups | Dispatch/required-status contracts red before implementation, 5 cases green after; existing publication refusals 8 cases green; all 21 guest invocations pass; repaired hosted gates pending | Guest 730.100s versus baseline 728.720s; initial hosted probe 894s wall / 1696 runner-seconds | Pending |

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

Guest evidence: `split-guest.json`, same Debian VM, Rust/profile/source tree and
serial workload as the investigation. All 20 Rust invocations and the two-test
Python invocation passed, including positive Debian certification. Sum:
730.100s wall / 646.180s user / 21.612s system. No fixture passwd/group entries
for 60000, 4019000 or 4020000, loaded LouiseLM units or matching processes
remained. This checks preservation, not the hosted parallel speedup.

Read-only cache-design investigation, not another optimization candidate:
`cache-freshness-probe.json` records three pairs of warm library-test builds
versus the same build after touching only `skills-core/src/lib.rs` in our guest
copy. The file's SHA-256 stayed
`bf27a9fbbde7ccbcedc57fabe9347bf2a4ac21c3add680dc5303a5a17e44284a`.
Warm wall samples: 0.150, 0.145, 0.085s; touched: 8.997, 4.074, 3.348s.
Cargo 1.97.1's fingerprint diagnostics explicitly mark that input stale.
This is a narrow timestamp probe, not a full fresh-checkout benchmark: a
source-key cache hit will not guarantee no compilation, though newer incremental
state may still reduce it. The hosted cache experiment must measure actual work.
