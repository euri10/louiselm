# Usage SQL optimization — louiselm-sawgo

Target: lower latency of the shipped picker/explorer queries, particularly broad
summary, grouped, turn-page and dimension reads. SQL remains canonical; preserve
validation, exact typed cohorts, sparse telemetry, mixed turns, per-currency costs,
paging, asynchronous scheduling and the existing timeout. No history deletion,
new host, dependency or public API is in scope.

Original revision: `c6e49ff60153a685c083c0417f512c63055d5e4a`. The relevant production
and test diff was empty. Pre-existing `.beads/issues.jsonl` changes and the
untracked blog directory belong to other work.

Maximum candidate passes: **3**. Consumed: **3**. No delegation.

Protocol: existing deterministic 2,518 / 25,180 / 251,800-turn workloads, five API
and CLI samples after warmup, four reused-connection samples. Same workstation,
executables and fixture generator as the baseline; run measurements sequentially
after gates. Retain raw JSON including min/max and exact result fingerprints.
The added optional profiler runs separately after measurements, recording
statement timings and `EXPLAIN QUERY PLAN`; its instrumentation is not in the
timed API path. Validate its 63-turn smoke before capturing the baseline.

Required gates: complete Lua suite, repository StyLua/LuaLS, four generated
artifact checks, plus fixture/result equality. This is a behavior-preserving
optimization: establish passing characterization coverage before candidates.

Baseline gates and 63-turn profiler smoke pass. Added characterization (11 focused
cases) passes before production changes. Baseline raw samples/plans: `baseline.json`.
At 251,800 turns the API medians / full sample spreads (ms) are summary
3820.55 / 165.75, grouped 2699.89 / 176.48, turns 2136.41 / 21.02 and dimensions
2492.78 / 43.07. The embedded diagnostic attributes 2260 ms of the summary to
metrics creation, with a window-sort/group plan over all cost readings. The
fixture has cost observations for only one fifth of turns.

Predeclared thresholds: an affected 100× broad query must improve by more than
**max(10% of its baseline median, its full baseline sample spread, 25 ms)**.
Compare each pass against both the original and previous accepted baseline.
Control queries/sizes must not regress by more than
**max(10% of their baseline median, their full baseline spread, 5 ms)**; the
absolute floor covers millisecond process/scheduling noise (the one-turn baseline
alone spans 1.80 ms). Exact result fingerprints and dataset/output sizes must agree.
No claim about unmeasured peak RSS is made. These thresholds precede production edits.

Pass 1 hypothesis: after validating every cost reading, omit baseline-only chains
from the window/delta calculation. They cannot satisfy its existing count > 1
condition. Keep complete chains, including NULL baselines and NULL readings, for
every turn that has a cost event. One causal lever in shared metrics; rollback is
deletion of that filter/comment only. This should reduce metrics work in all four
broad queries without changing validation or aggregate results.

| Pass | Hypothesis / lever | Base and candidate diff | Artifacts | Median/spread and delta | Gates | Decision |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Prune baseline-only chains after full validation | Original + `pass-1.patch` | `pass-1.json`, `pass-1-comparison.json` | Summary 3820.55 → 3676.90 ms (3.76%); grouped 3.48%, turns 3.83%, dimensions 1.14% | All gates pass; fingerprints/sizes match; zero repairs | **Reject**: every targeted gain is below the declared threshold. Removed only this pass's three SQL lines. |
| 2 | Reuse global totals for the ungrouped summary row | Original + `pass-2.patch` | `pass-2.json`, `pass-2-comparison.json` | Summary 3820.55 → 2044.04 ms (46.50%); spread 165.75 → 43.44 ms; filtered 37.57% faster | All gates pass; fingerprints/sizes match; controls within tolerance; zero repairs | **Keep**: clears gain/noise thresholds after the bounded measurement retry. |
| 3 | Drop SQL-NULL baselines without membership lookup | Pass 2 + remaining difference in cumulative `pass-3.patch` | `pass-3.json`, `pass-3-comparison.json`, `pass-3-original-comparison.json` | Versus pass 2: summary 9.97%, grouped 8.24%, turns 10.28%, dimensions 10.11%; versus original, grouped/turns/dimensions all below 10% | All gates pass; fingerprints/sizes match; zero repairs | **Reject**: fails the declared minimum for all targeted views against both baselines. Removed only this pass's three SQL lines. |

Stop reason: **3/3 candidate passes consumed**. Keep pass 2 only; both cost-window
experiments are reverted. Do not lower thresholds after seeing a nearly passing
result. `final-comparison.json` compares the retained state with the original.

## Retained result

Public API medians in milliseconds, original → retained pass 2:

| Query | 2,518 turns | 25,180 turns | 251,800 turns |
| --- | ---: | ---: | ---: |
| Default summary | 35.05 → 19.55 | 353.96 → 189.62 | 3820.55 → 2044.04 |
| Filtered summary | 4.50 → 4.51 | 13.99 → 11.57 | 89.58 → 55.92 |

The one production change is an ungrouped-summary branch that computes its
cohort once and uses the same metrics for totals and the single row. Empty
results and pagination remain unchanged. Grouped summaries, turn pages,
dimensions, picker and drilldown have no retained algorithm changes, and their
timings remain within the predeclared regression tolerance. This campaign does
not claim those paths became faster. SQL cost-chain logic and validation are
unchanged in the final implementation.

Final restoration matches `pass-2.patch` byte-for-byte. Candidate gate runs pass
**1,177 cases in 89 groups**, including the added baseline/NULL-chain and
empty/paged summary characterizations. All benchmark results, dataset shapes and
output byte sizes match the baseline at each measured size. No additional
candidate or measurement protocol change follows the rejected third pass.
Lesson destination: this ledger and `louiselm-sawgo`.
Removing a duplicate cohort is a demonstrated gain; both more elaborate and
cheap cost-window pruning failed the predeclared acceptance criteria.

Final shared-tree checks: StyLua, LuaLS, API appendix, LuaCATS and plugin-version
checks pass. The full suite now contains 1,179 cases/90 groups due to concurrent
Attention work; its sole failure is `tests/docs/vimdoc_spec.lua:55`, and the
Vimdoc check also reports stale help. The unrelated in-flight
`lua/louiselm/ui/chat/command.lua` diff adds `:LouiselmAttention` without generated
help yet. The usage cases pass. Do not regenerate another session's artifacts or
claim the current shared-tree suite is entirely green. These paths were untouched
by this campaign; its preceding complete gate runs were green.

Restoration after pass 1: production diff is empty again; all 11 focused query
characterization cases pass. Pass 2 starts from the original accepted baseline.
Hypothesis: the ungrouped summary's sole row is identical to its overall totals.
Use one cohort and reuse candidate 0 for that row, avoiding all duplicate metric
and validation work plus the unused grouping tables. Retain total=0 for an empty
cohort, total=1 otherwise, and respect limit/offset for the row independently of
overall totals. Only the 100× summary is a gain target; all other paths remain
regression controls under the predeclared tolerances. Rollback removes this one
new SQL-building branch; original grouped behavior remains below it.

Pass 2 measurement interruption: the first run returned the reader's generic
storage error during 100× capture (`pass-2-failure.log`); no final timings were
emitted. Free space was ample. Isolated captures of all eight unchanged queries
then succeeded (`diagnose_capture.py`, `capture-diagnostic.log`), including summary
at 2137.56 ms with Neovim startup. The cause is not established; do not claim it
was fixed or relax the production deadline. One bounded full measurement retry
uses the identical candidate and protocol. No candidate repair/reapplication or
threshold reset; a second incomplete run stops this campaign.

The full retry passed; its samples and comparison are retained. Diagnostic
follow-up `louiselm-tetp1` tracks missing failed-query/process evidence; the first
capture failure remains unexplained, not silently declared fixed.

Pass 3 starts from accepted pass 2. Hypothesis: avoid pass 1's membership-set
construction by dropping only SQL-NULL baseline rows from the cost window. Keep
every positive-sequence observation. A chain missing its baseline still fails:
its first retained observation has positive sequence and NULL previous amount.
JSON-null baselines remain untouched; NULL observations within chains remain
present. All validation still runs on the complete readings before this filter.
Read-only current-store check: 2105/2529 baselines were SQL NULL (83.2%, versus
80% in the synthetic fixture); no raw history retained. Existing and added tests
cover both forms of absent baseline, multiple subsequent readings, cleared
readings, measured zero and malformed baseline-only records. All 11 focused
cases passed before applying this candidate. This is the third and final pass,
including the rejected pruning attempt. Gain targets: all four broad 100× views
against both accepted pass 2 and the original baseline. Rollback removes only
the new window predicate/comment, preserving pass 2.

## Reproduction and gates

Use an isolated checkout of the original revision plus `harness.patch` to repeat
the baseline. Candidate patches are cumulative production diffs against that
revision; apply only the selected patch. Fixtures, executables and sample counts
must stay the same. Measurements run without other test/gate processes from this
campaign; this is an ordinary workstation, not an isolated performance host.
SQLite versions differ between the CLI and embedded profiler, as recorded in each
JSON. Decisions use the real Neovim API timings; profiler timings/plans explain
candidate selection rather than stand in for a production-engine speedup.

```sh
python3 docs/performance/usage-20260924/benchmark.py --sizes 63 --profile
python3 docs/performance/usage-20260924/benchmark.py --profile > /tmp/usage-sql-run.json
python3 docs/performance/usage-sql-sawgo/compare.py \
  docs/performance/usage-sql-sawgo/baseline.json /tmp/usage-sql-run.json \
  summary

stylua --check .
lua-language-server --check . --checklevel=Warning
nvim --headless --noplugin -u ./tests/minimal_init.lua \
  -c 'lua MiniTest.run()' -c 'qa!'
./scripts/generate-api-appendix --check
./scripts/generate-luacats --check
./scripts/generate-vimdoc --check
./scripts/generate-plugin-version --check
```

Comparator negative smoke: baseline compared with itself rejects the requested
summary gain (exit 1). Exact equality, resource-shape and environment checks run
before judging timing differences. No performance threshold is added to CI;
machine-dependent measurements accompany the existing deterministic correctness
suite. No raw production history is included in this campaign.
