# skills-core CI implementation campaign

Issues: `louiselm-ljf7w` (job split), `louiselm-edon3` (cache experiment).
The maintainer authorized both after the investigation in `ledger.md`.

Maximum candidate passes: **3**. Consumed: **1**, evaluation in progress.
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
| 1 | Two matrix VMs, serial `core` / `lifecycle` groups | Dispatch contract red before implementation, 4 cases green after; existing publication refusals 8 cases green; real guest/hosted gates pending | Pending | Pending |

The dispatch regression preserves the 21 original external invocations and their
gate flags, namespaces, masks, filters, timeouts and serial group selectors.
It executes fake Cargo/sudo commands only, tests empty selection and unknown
group refusal, and injects a failing fixture to prove its status propagates.
Both matrix jobs are unconditional; neither continues on error. `fail-fast`
is disabled so the sibling still supplies its gate evidence after a failure.
Release automation's existing failed-overall-CI refusal remains enforced.
