# Usage query host measurements — louiselm-9ytn

Keep the existing SQL/Lua execution boundary. Broad explorer queries have a
measured history-size cost worth tuning in SQL; these measurements do not justify
another owned Rust executable. Preserve the existing SQL definitions, exact typed
cohorts, coverage denominators, mixed-turn exclusions and per-currency results.
Do not reduce retention to make the benchmark smaller.

The SQLite CLI on this workstation comes from the Android SDK (3.50.6), whereas
Python links SQLite 3.46.1. Consequently, differences between their absolute
times include SQLite versions/builds and Python dispatch, not just host overhead.
The useful finding is that broad queries still take seconds without process
startup, while real process and output costs are millisecond-scale. Retaining a
connection did not remove that work. A same-engine Rust benchmark would be needed
before claiming a particular Rust speedup; none is claimed here.

## Results and decision

Measured on 2026-09-24, Ryzen AI 7 PRO 350, Linux 7.1.8, Neovim
0.13.0-dev-1511, Python 3.13.11; source revision
`6ef32dba3dccb48efdd83430733bc859fb9955b8`. This is the maintainer's installed
Neovim, not a claim about stable-version timings. `results.json` retains every
sample and the complete environment. Unrelated concurrent worktree changes were
preserved; the measured production Lua files were not changed by this task.

All datasets retain 9 Agents, 10 Providers, 23 Models and 63 typed option tuples:

| Dataset | Turns | Sessions | UTC days | DB bytes | Total-token coverage |
| --- | ---: | ---: | ---: | ---: | ---: |
| Current snapshot | 2,519 | 348 | 18 | 2,887,680 | 2,456/2,519 |
| Synthetic 1× | 2,518 | 315 | 18 | 1,449,984 | 2,450/2,518 |
| Synthetic 10× | 25,180 | 3,148 | 175 | 14,176,256 | 24,501/25,180 |
| Synthetic 100× | 251,800 | 31,475 | 1,749 | 146,071,552 | 245,017/251,800 |

Median public API latency in milliseconds, five repetitions:

| Query | Current | Synthetic 1× | 10× | 100× |
| --- | ---: | ---: | ---: | ---: |
| Picker cohorts | 12.32 | 3.50 | 14.62 | 139.98 |
| Default summary | 50.31 | 38.21 | 359.14 | 3,945.78 |
| Daily Agent/Model/option groups | 43.03 | 36.02 | 251.04 | 2,712.04 |
| Joint filtered summary | 17.36 | 5.46 | 14.97 | 85.73 |
| First 25 turns | 43.40 | 30.89 | 200.36 | 2,196.24 |
| Dimension discovery | 34.70 | 23.72 | 223.54 | 2,545.70 |
| Session events | 4.87 | 3.82 | 6.62 | 26.35 |
| One turn's events | 3.58 | 3.16 | 2.47 | 3.51 |

For the 100× default summary, API samples range from **3,921.62–4,381.37 ms**.
The separate CLI median is 3,915.16 ms; embedded SQL is 4,361.53 ms on fresh
connections and 4,583.28 ms on reused connections. The latter is not faster on
this run. The existing reader timeout is 5,000 ms, so broad queries have limited
headroom at this size even though all measured calls completed.

The `sqlite3 :memory: SELECT 1` launch/open/execute baseline is **1.61 ms**
(0.97–2.16 ms). Result bodies stay between 621 and 21,721 bytes across the full
run. Process-plus-result replay medians are 1.48–2.89 ms; empty `cat` is 1.65 ms.
Negative differences at this scale are noise, not negative transport cost.
Median Lua decode times are at most **0.101 ms** and headless explorer render
times at most **0.216 ms**. Those costs cannot explain multi-second broad reads.

The current picker and explorer do not demonstrate an execution-host problem.
Growing history does demonstrate costly broad SQL work even with small output
pages. Narrow filters and turn drilldown provide useful controls: the 100×
filtered summary is 85.73 ms and one-turn events remain 3.51 ms. No performance
threshold was invented for closure, and warm filesystem-cache results do not
certify cold-disk or concurrent-writer tail latency.

Implementation follow-up **louiselm-sawgo** records the evidence and requires
profiling and reducing broad SQL work with the same exact results and repeated
measurements. Candidate materialization, validation and aggregation are the next
places to measure; this report does not claim an untested rewrite or index works.
No Rust host or retention change is scoped. Lesson destination: this report and
the measurement issue, not a new standing architecture rule.

## Reproduce

From the repository root, using the existing Python, Neovim and SQLite tools:

```sh
python3 docs/performance/usage-20260924/benchmark.py > /tmp/usage-benchmark.json
# Optional: measure a private read-only snapshot of the current recorder too.
python3 docs/performance/usage-20260924/benchmark.py \
  --current /home/lotso/.local/state/louiselm/usage/turns.sqlite3 \
  > /tmp/usage-benchmark-current.json
# Small correctness smoke, including sparse telemetry and a mixed turn:
python3 docs/performance/usage-20260924/benchmark.py --sizes 63
```

The current database is opened read-only and backed up into a private temporary
directory. Captured SQL/results and snapshots stay there and are removed on exit.
The committed JSON contains only timings, output sizes, environment and aggregate
dataset shape. No prompts, option values, Session identities or credentials are
published. No live buffer, recording settings, retention or database is changed.

## Workloads and measurement boundaries

The harness calls the shipped `RecordingStore:usage_summaries` (picker) and
`RecordingStore:usage_query` (explorer), including their validation and snapshot
guards. It captures their exact SQL once through a temporary executable on PATH;
timed calls use the real executable. No production API or Neovim function is
replaced, and no copied SQL implementation supplies the measurement.

Picker candidates vary the Model while holding the other options and Provider
fixed, using up to twelve recorded Models plus the most frequent starting tuple
if missing. This measures the store query used by the picker, excluding pending
recording flushes, Provider resolution, user input and the configured
`vim.ui.select` provider. Explorer cases cover default totals, Agent/Model/option
daily groups, joint typed filters with a UTC lower bound, the first 25 turns,
dimension discovery, a Session event timeline and one turn's events.

- `api_ms`: real asynchronous reader invocation to callback, including SQL
  construction, permissions, process launch, SQL, transport and Lua decoding.
- `sqlite_cli_ms`: captured SQL through the real CLI, stdout drained into Python;
  includes process launch, database work, serialization and transport.
- `sqlite_fresh_connection_ms`: the same SQL through Python's existing SQLite
  binding, with a new connection per sample; timer starts after opening it.
  This is **cold SQLite page cache with warm OS filesystem cache**, not cold disk.
- `sqlite_reused_connection_ms`: the same SQL on a retained connection, after
  dropping temporary tables outside the timer; the first iteration is excluded.
  Both embedded measurements include statement dispatch/fetch and SQL-generated
  JSON, but exclude process startup and CLI stdout serialization. They are an
  execution-host comparison, **not a benchmark of an unimplemented Rust host**.
- `cat_process_and_transfer_ms`: replay the captured result through `cat` and a
  pipe. Compare with `cat_empty_process_ms`; this includes process/file overhead
  and does not pretend subtraction of noisy medians precisely isolates transfer.
- `decode_ms`: actual Neovim JSON decoding (both explorer JSON layers), averaged
  over 100 decodes per sample. Picker metric-to-summary mapping remains in API time.
- `render_ms`: actual explorer `set_query` through its scheduled rendering,
  injecting an already obtained page at its public reader boundary. Includes the
  loading render, copying query state, scheduling and buffer updates. Headless
  rendering excludes terminal painting, third-party highlighting and picker UI.

There are five measured repetitions after an API warmup; warm-connection SQL has
four samples after its first execution. Raw runs, median and min/max are retained.
Runs are sequential on a working workstation, without CPU pinning, global cache
flushing or a claim of isolated-host precision. SQL versions are recorded separately.

## Fixture contract and correctness

The recorder itself creates the schema. Deterministic normalized facts then fill
it without invoking an Agent: 9 Agents, 10 Providers, 23 Models and 63 complete
typed option tuples. Options include absent fields, booleans and the string
`"false"`. Sessions grow at one per eight turns; timestamps advance ten minutes
per turn (144/day), so time-group cardinality grows with history. These are
synthetic normalized facts, not claimed ACP observations or a forecast of usage.

Every turn dispatches; every 503rd has no outcome, every 40th has no token report,
and output/thought token coverage differs from total/input coverage. Thought
tokens include measured zero. Every fifth turn has a cost baseline and increment,
split between EUR/USD. Every 997th turn changes options during the turn; every
third has a between-turn transition. This approximates the current sparse
telemetry while preserving distinctions that a fast but incorrect query could lose.

Checks assert known synthetic counts, totals, coverage, unobserved outcomes and
mixed turns. All CLI and embedded results must compare equal, including nested
JSON, cohorts and metric coverage. Every repeated API result compares exactly in
memory. The separate capture/API comparison round-trips both sides through
Neovim's same JSON encoder because it rounds floating-point values. The existing
SQL tests retain the broader currency-reset, typed-filter and malformed-input
contracts; no alternate aggregation math is proposed for production.

This is measurement code with executable self-checks, not a runtime behavior
change requiring an artificial failing product test.

Verification: the small synthetic self-check and all full-size comparisons pass;
the complete Lua suite passes 1,175 cases in 89 groups, with zero failures/notes.
Repository-wide StyLua and LuaLS checks pass. No product code, public API,
configuration, dependency or generated documentation changes are needed.
