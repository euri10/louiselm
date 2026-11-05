# Query the existing factual boundary

Requires Neovim, sqlite3 >= 3.38 with JSON, and the maintained LouiseLM checkout
(resolve its path from the plugin's runtime path, or ask). Resolve this skill's
own directory from its discovered path. The wrapper runs in a standalone child, never via `dofile`
in the operator's editor. It only constructs a reader and calls `usage_query`;
it does not flush pending writes, migrate, or collect anything.

```sh
nvim --headless --noplugin -u NONE -l /absolute/skill/scripts/query.lua \
  /absolute/louiselm - \
  '{"from":"2026-09-07T00:00:00Z","until_time":"2026-09-08T00:00:00Z","limit":10}'
```

Pass the absolute usage **directory**, not the database file, instead of `-`
when an explicit scope uses another store. `-` resolves the documented shared
state root: `$XDG_STATE_HOME/louiselm/usage` or
`~/.local/state/louiselm/usage`. Confirm an explicitly different live store path
from known configuration, never by searching history directories. See the
checkout's `docs/turn-recording.md` for the underlying contract.

Exit 0 emits `{query,directory,store_present,read_at,page}`; exit 1 emits a typed
`error`. `store_present=false` means no database was observed, not zero recorded
activity. Query errors are not empty cohorts. Preserve nulls and booleans when
passing JSON as one literal argument. Do not interpolate recorded strings into
shell/Lua; use an argument-array process call or proper shell quoting.

## Query shapes

Carry the chosen range/filters into each query unless the purpose requires a
different scope; state any such change. These are JSON query examples, not new
query implementations:

| Purpose | Query additions |
| --- | --- |
| Overall totals | `{"view":"summary"}` |
| Recorded dimensions | `{"view":"dimensions","limit":10}` |
| Selected configuration groups | `{"view":"summary","group_by":["agent","provider","model","option:effort"]}` |
| Turn references, including mixed | `{"view":"turns","limit":10}` |
| Mixed turns | `{"view":"turns","mixed":"only","limit":10}` |
| One turn's observations | `{"view":"events","turn_id":"DURABLE_ID","limit":10}` |
| Session timeline, including idle changes | `{"view":"events","filters":{"agent":"AGENT","session":"ACP_ID"},"limit":10}` |

Replace illustrative dimension/ID values with observed ones. Filters are a
conjunction: `{"option:enabled":false}` differs from `"false"`; null matches
an absent dimension. Hour/day buckets and ranges are UTC; turn queries use
`prepared_at`, event queries use `observed_at`. Event-page summaries describe
selected dispatched turns, not just turns prepared inside the event window.
Use a separate summary query for time-scoped turn totals.

`page.total` counts rows for the chosen view (groups, turns, events or dimension
values); it is not always a turn count. `page.summary.turns` is the dispatched
turn count. `summary.outcomes` includes `unobserved`; each `tokens[FIELD]` and
`costs[]` metric has `samples`, `average`, `total`, with costs keyed by currency.
Only measured samples contribute. Missing keys mean no measurements, not zero.
`mixed_turns` and `excluded_mixed` describe the selection. Summaries filtered or
grouped by Provider/Model/options exclude mixed turns automatically.

Event rows have `kind`, `id`, `turn_id?`, `observed_at`, `sequence`, and
`observer_id?`. `kind="options"` has `data.agent`, `data.acp_session_id`,
`previous_options`, `options`, `source` and nullable `request={id,option,value}`.
Diff both snapshots, including added/removed keys; attribute only the requested
field to the explicit request. An unfiltered Agent/Session event query includes
idle changes. Provider/Model/option filters or a turn ID exclude idle changes:
remove those filters for that separate timeline query and disclose the scope.

Read one page at a time (default 25, allowed 1–100), project the envelope and
needed fields before tool output if necessary. A summary is already aggregated
over the selection, not just displayed rows. Query a few candidate Sessions in
detail; do not dump an unlimited history. If a bound or error prevents the needed
inspection, report the gap rather than interpreting truncated JSON.
