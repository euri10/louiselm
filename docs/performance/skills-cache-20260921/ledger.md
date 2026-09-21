# Cargo cache A/B — louiselm-edon3

Authorized 2026-09-21: one candidate on a temporary GitHub branch; do not
change either remote's main branch. This uses **pass 3** of the earlier
three-pass campaign, not a reset. Maximum total **3**, consumed **2**;
remaining **1**. At most **2** same-lever repairs. No candidate applied yet.
Maximum new hosted executions: **10**, stopping early on failure/no gain.

## Input and scope

- Baseline: `c4c12245d9b753817549313bc14745e52be88a4f`, clean source tree;
  identical file contents to accepted optimization `fd00845`.
- Isolated worktree: `/var/tmp/louiselm-cache-ab.75EdGx/repo`, branch
  `codex/skills-cache-ab-20260921`. Shared tracker updates are attributable to
  `codex/01a0c1d7-bbcf-7f61-88dd-6b36b225272f`.
- Only proposed causal lever: skills-core Cargo cache key/restore selection.
  Preserve compiler/profile, Cargo fingerprints, all jobs, test bodies,
  privileged isolation, assertions, timeouts and release guards. No sharding,
  dependencies, timestamp manipulation, main push or cache deletion.
- Existing evidence: `../skills-ci-20260918/cache-freshness-probe.json` proves
  newer checkout timestamps can require recompilation despite unchanged bytes.
  This experiment measures actual fresh GitHub checkouts, not same-directory
  local warm builds. Existing caches are not trusted release artifacts.

## Protocol fixed before applying the candidate

1. Run the unmodified full CI workflow three times at the exact baseline SHA,
   on distinct temporary refs to avoid concurrency cancellation. Each restores
   the existing main-scoped cache. Record job/step timings, cache key/bytes,
   compile diagnostics, runner CPU/kernel/RAM and all gate conclusions.
2. Reuse `../skills-ci-20260918/collect-hosted.py`; validate it against the
   existing successful merged run before interpreting new results. Keep raw
   projected evidence, commands and exact revisions here, not complete logs.
3. Before applying the candidate, record median/range and numeric threshold.
   Acceptance requires a median skills-core job reduction greater than **10%**,
   **20 seconds**, and **twice the larger baseline/candidate range**. All
   included jobs must pass; different CPU cohorts or unresolved noise cannot
   justify a speedup claim. Include all job setup, restore, build, test and
   post-job save time; report queue delay separately.
4. Apply one minimal source/manifest-aware key with compatible restore prefixes.
   The first full candidate run is its correctness gate/cache seed, not a warm
   measurement. Only after all gates pass, run three serial dispatches on that
   same candidate ref/SHA: GitHub caches are branch scoped, so sibling refs
   must not be treated as sharing the candidate's saved cache.
5. Stop/reject if warm evidence fails the threshold. Only if it passes, use
   the three remaining executions for a source-changed warm run (a comment-only
   crate-source change) and one cold baseline/candidate pair. Cold probes use
   isolated absent cache namespaces, never delete shared caches. Include cache
   save/upload and bytes; report single-run cold/change observations as such,
   not repeated estimates. Reject a material unresolved cost regression.
6. Preserve all gate coverage. A failed baseline stops before candidate
   application. A candidate correctness failure stops with its evidence; no
   unrelated reliability repair or rerun-until-green is authorized.

The budget is at most 3 baseline + 1 seed + 3 warm + 1 changed-source +
2 cold = 10 executions. Existing completed runs used for collector validation
do not consume executions. No automatic retries for noisy or failed samples.

## Candidate ledger

| Pass | Lever | Exact input | Gates | Timing / cost | Decision |
| --- | --- | --- | --- | --- | --- |
| — | Unchanged hosted baseline | c4c1224, no source diff | Android dependency resolution failed; remaining work cancelled | Incomplete, excluded | Stop before candidate application |

## Current status

**Stopped before candidate application.** Historical consumed passes remain
**2 of 3**; this attempt dispatched **3 of at most 10** executions, all now
completed/cancelled. No gain threshold can be computed from incomplete
baselines. No cache change, candidate commit, main push, dependency installation
or repository setting change occurred. Retained performance delta: none.

## Baseline failure and preserved evidence

The existing collector was validated against successful merged run
`35562245873`: all 13 jobs pass, skills-core 343s. See
`collector-validation.json`. This is collector validation, not a replacement
for the planned three exact-revision baseline dispatches.

| Ref | Run | Outcome |
| --- | --- | --- |
| codex/cache-base-20260921-1 | 35563238799 | Cancelled after sibling baseline failure |
| codex/cache-base-20260921-2 | 35563238692 | Cancelled after sibling baseline failure |
| codex/cache-base-20260921-3 | 35563239473 | Android failed; remaining jobs cancelled |

All refs point to the unchanged baseline SHA. Dispatch command:
`gh workflow run ci.yml --repo euri10/louiselm --ref <ref>`.
Only these owned benchmark runs were cancelled; unrelated CI was not touched.

[Android job 106219976493](https://github.com/euri10/louiselm/actions/runs/35563239473/job/106219976493)
fails during Gradle configuration, before application tests:

```text
Could not resolve all artifacts for configuration 'classpath'.
Could not GET 'https://repo.maven.apache.org/maven2/com/google/code/gson/gson/2.11.0/gson-2.11.0.pom'. Received status code 429 from server: Too Many Requests
BUILD FAILED in 20s
```

Other POM requests fail with the same status. Endpoint rate limiting is observed;
its trigger is unknown. Parallel cold jobs may add request pressure, but this
run does not establish that causal link. Do not describe this as an Android
test failure, Rust regression, or cache-candidate failure.

Filed **louiselm-cbc5d (P2)** before any repair; it blocks this experiment.
No Android repair or retry-until-green was attempted. The profiling skill's
failed-baseline stop rule ends this attempt, not the unused execution budget.
Further reliability work/resumption needs maintainer direction.

Artifacts:

- `android-baseline-failure.log`: bounded exact failing log excerpts.
- `baseline-failure.json`: failed Android job/step metadata.
- `baseline-stopped.json`: final gate conclusions and available partial skills
  diagnostics for all three cancelled workflows; their timings are **not**
  accepted samples.

The candidate branch retains evidence only. Its `.github/workflows/ci.yml`,
`skills-core` and `android` remain byte-for-byte unchanged from the baseline.
The original main worktree only has the shared tracker export update; neither
local main nor either remote main was advanced by this attempt.
