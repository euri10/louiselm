---
name: louiselm-profile-driven-optimization
description: Profile and optimize a chosen performance hotspot through bounded simplification, single-lever changes, regression gates, and measured comparisons. Use for an optimization campaign, not routine feature work.
license: MIT
---

# LouiseLM profile-driven optimization

An agent-executed procedure for LouiseLM; these instructions do not implement
runtime budget enforcement or install the upstream slash commands.

## Scope and bounds

- Select the requested target, representative workload, metric and direction
  (lower latency/allocation or higher throughput), and applicable correctness gates.
  If no target can be inferred, ask before changing code.
- Default maximum: **3 candidate passes total**, including pruning, failed
  candidates, and retries of rejected candidates. Local repairs within the same
  candidate follow step 3 below. Honor an explicit user bound and any smaller
  remaining Run budget. Record a literal integer before starting; never reset it
  on handoff.
- Stop at that maximum, budget exhaustion, operator interruption, an unresolved
  correctness failure, or two consecutive candidates without a measurable gain.
  Stop earlier if no evidence-backed candidate remains.
- Keep changes within the target. Preserve unrelated work. This skill does not
  authorize commits, dependency installation, new Sessions, or delegated work.
  When commits are authorized, keep one causal lever per commit.

## Baseline before changes

1. Read the applicable repository contract and inspect the target and its callers.
   Record the starting commit plus the exact relevant dirty diff, if any. A commit
   hash alone cannot identify a dirty benchmark input.
2. Run the applicable characterization tests and gates. If they fail, record the
   failure and stop; do not optimize a failing baseline.
   Validate a new benchmark harness before applying a production candidate;
   repair harness diagnostics first, then capture the baseline with that version.
3. Capture a repeatable baseline with an existing benchmark or profiler. Record
   commands, tool versions, build flags, input/seed, warmups, sample count, machine
   conditions, raw samples, median and spread. Test suite duration alone does not
   identify a hotspot; use target-specific timing or profile attribution.
4. Before testing candidates, choose a minimum useful gain and a noise threshold
   from repeated baseline samples. Require gain above both thresholds; a fixed
   percentage alone does not prove statistical significance. If measurement is
   too noisy to distinguish changes, stop with that limitation.

## Simplify, then optimize

Use the available Ponytail skill for simplification: first consider deleting
unneeded work, reusing existing code, or using native facilities. Audit only the
chosen target. A read-only audit consumes no candidate pass; every applied pruning
candidate does. Skip pruning when no justified simplification exists.

Caveman governs concise reports and commit/review wording, not code pruning.
Use the applicable available Caveman suite skill when requested; keep commands,
measurements, failure evidence and reproduction details complete. If either suite
is unavailable, follow the principles above without installing dependencies.

For each candidate, sequentially:

1. Write one hypothesis tied to baseline evidence. Choose one causal lever and
   record the exact starting state and a rollback limited to your own changes.
2. Apply the smallest change. Preserve validation, errors, protocol behavior,
   asynchronous scheduling, lifecycle/disposal and security boundaries.
3. Run focused checks and all affected gates required by the repository.
   A local, understood implementation diagnostic (such as formatting or a type
   annotation mismatch) may be repaired within the same candidate when the fix
   preserves its hypothesis, causal lever and scope. Allow at most **2 repair
   rounds per candidate**, within the remaining budget; rerun affected checks
   after each repair. Never weaken tests, validation or diagnostics to pass.
   If correctness remains unresolved, the failure requires a different behavior
   or scope, or the repair allowance is exhausted, undo only this candidate,
   verify the restored state, record the failing evidence and stop. Do not
   measure or accept a candidate until all required gates pass.
4. Measure the same workload using the baseline protocol. Keep environment,
   build mode and sampling comparable; if they change, the comparison is invalid.
   Compare against both the previous accepted state and the original baseline.
5. Keep only candidates clearing the predeclared gain/noise thresholds with no
   correctness or measured resource regression. Otherwise undo only this
   candidate, verify restoration, and record negative evidence. Never use a
   blanket reset, checkout or stash in a shared tree.
6. Update the ledger before continuing. An accepted candidate becomes the next
   baseline; a rejected candidate leaves the previous baseline unchanged.
   Check remaining bounds before beginning another candidate.

Pruning follows these same measurement and acceptance rules. A readability-only
cleanup may be proposed separately, but is not counted as a performance gain.

## Durable evidence

For every campaign, including one pass, keep a ledger and raw artifacts in a
repository-local, campaign-specific directory; link it from the project tracker
when one is in use.
Avoid a shared root ledger that concurrent campaigns overwrite. Follow the
repository's attribution and issue workflow. Do not store prompts, credentials,
environments or sensitive payloads in benchmark artifacts.

The ledger header records target, workload, original revision/diff, gates, metric
direction, gain/noise thresholds, sample protocol, maximum passes, and consumed
passes. Each candidate row records:

| Pass | Hypothesis / lever | Base and candidate revision/diff | Artifact paths | Base / candidate median and spread | Delta | Gate results | Keep / reject and reason |
|---|---|---|---|---|---|---|---|

Retain raw baseline/candidate samples, profiler output, relevant gate results and
candidate patch identity even for rejected work. Record the final stop reason,
consumed budget, retained changes, and original-to-final measured delta. Report
uncertainty explicitly; discovery and parsing checks are not performance evidence.
Keep gate commands and outcomes concise; retain relevant failure diagnostics,
not repetitive successful logs. Do not create empty artifact files for checks
that produce no output.
