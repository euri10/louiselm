# Durable turn recording

Every production Session, including headless Sessions, records turn metadata in
`$XDG_STATE_HOME/louiselm/usage/turns.sqlite3`, falling back to
`~/.local/state/louiselm/usage/turns.sqlite3` when the variable is unset or empty.
This is shared across Neovim profiles (`NVIM_APPNAME`). There is no opt-out. Headless
owners may choose an absolute private directory with `Session.new`'s third
argument, `{ usage_directory = path }`.

All shared LouiseLM state uses the same root, independently of the editor profile:

| Relative path | Owner and purpose |
| --- | --- |
| `usage/turns.sqlite3` | Session registry: durable turn and option facts |
| `usage.json` | Chat: read-only legacy replay annotations |
| `forensics/` | Session registry: immutable Forensics records |
| `permissions.json` | Permission store: remembered rules |
| `abandoned.json`, `routing-evidence.json` | Neovim: abandonment breadcrumb and routing evidence |
| `capture-recordings/` | Neovim: private recording scratch, removed after successful ingestion |
| `capture/`, `workflow/` | Capture service: pairing/TLS, uploads, Runs and Attention |
| `skills/` | Skills tool: default trusted store |

Explicit headless store paths take precedence. `LOUISELM_CAPTURE_STATE_DIR`
overrides the service's state base (including the Neovim workflow clients);
`LOUISELM_SKILLS_STORE` overrides the complete skills-store path. These are
component-specific overrides, not aliases for the shared root.
Configuration stays in its existing configuration directory, and canonical
capture audio stays beneath `$XDG_DATA_HOME/louiselm/captures`.

Older builds used `stdpath("state")/louiselm` for Neovim-owned state. Moving
existing records is an explicit operator action, never an import/setup side
effect. Settle all old writers before transferring their state, preserve
private permissions and a recoverable backup, and resolve existing destination
files before copying. A running editor retains its existing store paths until
its owners are disposed or explicitly transferred; installing updated files
does not change live objects. Do not turn QA backups into active permission rules.

`sqlite3` >= 3.38 with JSON support is required on `PATH`. CI installs it and
tests use real temporary databases. Each connection verifies the version, JSON
functions, foreign keys, schema version, DELETE journal mode, and synchronous
EXTRA. The development executable tested initially was SQLite 3.50.6. WAL is
not used. [SQLite's synchronous documentation](https://sqlite.org/pragma.html#pragma_synchronous)
describes the additional directory synchronization provided by EXTRA with DELETE
journals; the [WAL documentation](https://sqlite.org/wal.html#the_wal_reset_bug)
records the separate WAL reset defect affecting that installed version.

## Admission and lifecycle

`session:prompt(content, callback)` returns a random 128-bit hexadecimal turn
ID, or `nil, message` for immediate validation/Provider/identity failures. The
ID is **not an ACP request ID or evidence of peer receipt**. Once accepted, the
Session is `preparing` and rejects another prompt/configuration request until
admission ends. The callback runs once on completion or asynchronous failure;
Disposal suppresses later prompt callbacks as before.

Before dispatch, the registry's recorder commits all earlier queued facts and
the new immutable start row. Only its successful acknowledgement permits the
ACP prompt write. A failed admission leaves the Session `ready`, calls the
callback with an error, and sends no prompt. Accepted attempts advance the local
turn ordinal even if admission fails; consumers must use the durable ID for
identity. Chat shows the submitted attempt and the recording error. Retrying
the prompt creates another attempt, with another ID.

Immediately before the ACP write, the Session checks the options, Model,
resolved Provider, and cumulative-cost baseline against the committed snapshot.
A change during admission rejects the attempt with `prompt_rejected` rather
than sending under stale attribution. Chat renders that event as “Prompt not
sent”; the operator can submit again using the current configuration.

The lifecycle publishes `preparing` only after queuing its start row. An observer
may synchronously Cancel or Dispose there: the terminal observation follows
the start row, and the pending callback cannot dispatch or revive the Session.
Cancellation after dispatch is a request, not proof of completion. A terminal
ACP response supplies completion/failed/cancelled evidence; transport loss and
Disposal explicitly record that no terminal response was observed. Prompt silence
alone never ends a turn or produces a failed outcome; it remains active until
the peer responds, an explicit error occurs, or the operator disposes it.

Agent-reported terminal failures (AIR `sessionFailure` metadata on the prompt
response) record `failed` with `peer_response=true`, including any reported
usage. The prompt callback receives `nil, message`; an `error` event replaces
`turn_done`. The ACP connection stays available, and the Session returns to
`ready` once the Agent is idle and permission requests are settled. Chat keeps
the diagnostic visible and clears queued follow-up work. The operator can
retry or select another advertised Model; neither happens automatically.

Codex's preceding `systemError` notification marks the turn as failed but does
not terminate its process before the detailed response arrives. A response
without detailed failure metadata still reports the generic Codex turn error.
Actual protocol and process failures continue to terminate the Session.

Recording failures publish `recording_changed` with a typed, sanitized error and
pending-write state. `Session:inspect()` exposes the same fields. Active work
continues; subsequent prompt admission must first flush the failed queue.
`api:flush_recording(function(err) ... end)` retries without starting work and
also supports acknowledging final facts after registry Disposal. It never blocks
Neovim. Do not quit the editor before its callback if final-write confirmation
is required.

Each SQLite process has a 1-second lock wait and a 5-second process bound.
Acknowledgements and recording observers run on Neovim's main loop. Disposal
does not kill an already queued final write: the bounded operation drains
independently, and its callbacks cannot revive a removed Session. If the editor
exits or storage remains broken, only committed facts survive. A missing
terminal fact means **completion unobserved**, including after resume; loading
history never invents a completion or reuses an old turn ID.

## Schema version 2

The writer upgrades version 1 transactionally by adding `option_events`; existing
turn facts remain untouched. Older writers refuse version 2. Restart other
editors using an older plugin before continuing to record into this database.

`turns` contains immutable initial records:

| Column | Meaning |
| --- | --- |
| `id` | Durable turn ID; primary key |
| `agent`, `provider`, `acp_session_id` | Explicit prompt-start attribution |
| `prepared_at` | UTC preparation timestamp |
| `options` | Canonical JSON object with typed string/boolean values |
| `model` | JSON string/boolean, or SQL NULL if unadvertised |
| `cost_baseline` | Last reported cumulative `{amount,currency}`, or SQL NULL |

`turn_events` contains immutable ordered observations, keyed by
`(turn_id, sequence)` with a foreign key to `turns`. Sequence starts at 1.
`observed_at` is UTC; sequence establishes order even when timestamps tie.

| `kind` | JSON `data` |
| --- | --- |
| `dispatch` | `request_id`: the locally accepted ACP write; `transcript_turn`: observed replay position; neither proves peer receipt |
| `cost` | `cost`: normalized cumulative amount/currency, or explicit JSON null |
| `cancel_requested` | Empty object; local cancellation request succeeded |
| `outcome` | `outcome`, `peer_response`, optional `usage` |

Outcomes are `completed`, `cancelled`, `failed`, `disposed`, or `not_sent`.
`peer_response` distinguishes a received terminal ACP response from local
termination/admission failure. There can be at most one outcome per turn.
Prepared rows alone are attempts, not proven started work. A dispatched turn
without a terminal observation has unknown completion. Neither timestamp nor
process disappearance proves successful completion.

`usage` preserves every supported reported token field: `total_tokens`,
`input_tokens`, `output_tokens`, `thought_tokens`, `cached_read_tokens`, and
`cached_write_tokens`. Zero is retained when reported; missing fields remain
absent. Context occupancy is never substituted for consumption.

Cost observations preserve the baseline, every accepted active-turn cumulative
reading, currency changes, decreases/resets, and explicit clearing. No per-turn
price is guessed. Consumers can derive a complete delta only from an observed
baseline, at least one in-turn reading, and an uninterrupted same-currency,
nondecreasing sequence. First
readings, a reset, clearing, or currency switch make a whole-turn delta
unavailable; do not treat a later partial delta as the whole turn or sum
currencies. Late idle readings can become the next turn's baseline but are not
retroactively attributed to a finished turn.

## Confirmed option history

`option_events` records each accepted change to the complete typed option tuple,
including changes made without any prompt. It records additions and removals as
well as value changes. Display-name changes and unchanged advertisements add no
events. Initial configuration and notifications during `session/load` establish
the baseline; replay never recollects historical transitions.

| Column | Meaning |
| --- | --- |
| `id` | Stable observation ID, unchanged when a failed write is retried |
| `observer_id`, `sequence` | Random live Session stream identity and its increasing observation order |
| `agent`, `acp_session_id` | Session whose confirmed configuration changed |
| `observed_at` | UTC observation time; sequence orders ties within one stream |
| `previous_options`, `options` | Canonical JSON objects before and after the accepted replacement |
| `source` | `notification` or `response`, describing the observed ACP source |
| `request` | JSON `{id,option,value}` for a matched response; otherwise SQL NULL |
| `turn_id` | Affected active attempt, or SQL NULL while idle |

Every resumed/live observer gets a distinct stream identity. Stream sequence
states observed order; it does not impose a global causal order on independent
editors. Both snapshots are captured and queued before Session events publish
the accepted state, so later mutable state and reentrant callbacks cannot
rewrite or overtake an observation.

A response records only its explicitly requested option/value as requested.
Additional changed options remain observations. A notification has no request
link, even while a request is pending. If a notification already confirmed the
whole change, an identical response adds no event or retrospective request link.
Failed requests do not themselves create transitions. No timing-based cause or
operator motive is inferred.

An active attempt retains its immutable prompt-start tuple. Any associated
`option_events` row makes that attempt unsuitable for comparisons claiming one
fixed option combination, even if the values later return to their starting
values. `Session:inspect().turn_options_changed` exposes the same fact for the
current/latest attempt and resets on the next attempt. Consumers exclude these
turns from fixed-combination comparisons with `NOT EXISTS (SELECT 1 FROM
option_events o WHERE o.turn_id = turns.id)`; they remain in overall usage and
outcome counts. Do not estimate how consumption splits across configurations.

Option-write failures use the existing durable retry/admission barrier and do
not undo the configuration the Agent actually accepted. An unresolved Provider
adds a Session-local `recording_error` with code `attribution`; successful storage
alone cannot clear it. A confirmed valid configuration clears that error, and
prompt admission still requires all earlier writes to commit. Other Sessions
are unaffected by this attribution error. Option events carry no guessed
Provider: resolved Provider attribution remains part of each turn's start row.

## Integrity and scope

The recorder is owned by the Session registry. It serializes local writes;
SQLite transactions coordinate independent registries/editors. Duplicate
acknowledgements retry the same encoded ID/sequence/content. Identical facts
are idempotent; conflicting facts fail the transaction and never overwrite
history. Failed writes stay queued. Invalid metadata fences that writer rather
than allowing a successful admission with no start row.

The directory must be owned and mode 0700; the database must be an owned regular
0600 file with one link; existing SQLite sidecars must meet the same checks.
SQL values are hex-encoded text literals; caller
strings never become SQL identifiers, SQL grammar, shell text, or CLI commands.
The CLI receives an argument array and SQL on stdin, with user startup scripts
disabled. Errors contain neither SQL nor record contents. No prompt, tool,
environment, or raw error payload is stored. Facts have no automatic expiry or
lossy rollup.

## Replay annotations

The Session lifecycle counts historical user turns during `session/load`, grouping
consecutive user chunks, including non-text content. Intermediate historical state
updates do not end replay. Each successful local prompt dispatch records its
`transcript_turn` after the observed history; failed admission and cancellation
before dispatch consume no transcript position. The random durable turn ID remains
the identity of the fact. Loading the same Session never records its history again.

`session:usage_history(callback)` asynchronously reads committed associations for
exactly that Agent and ACP Session, returning `{id, turn, usage?}` records on the
main loop. The known ACP ID is available before load initialization finishes.
It first acknowledges any pending writes owned by this registry, so an immediate
reload cannot outrun the previous completion. The underlying store read creates
no database and migrates no history; missing storage returns an empty list and
other failures return a typed error.

Chat waits for this read before presenting queued replay events, preserving their
immutable status and order. It shows exact SQLite usage beside the associated
turn and uses exact entries from the pre-rollout `usage.json` only where no new
association exists. A new unmeasured turn or several durable IDs at one position
suppresses fallback rather than guessing. A read failure is visible and does not
prevent rendering the available history. Queued callbacks cannot revive a disposed
view; live completion snapshots retain their own measurements even if a later turn
has already started before the UI callback runs.

Old SQLite dispatches that lack `transcript_turn` remain unassociated; timestamps
and attempt counts cannot reconstruct that evidence. External changes to an Agent's
history (truncation/reordering, or concurrently prompting the same ACP Session)
cannot be made lossless by client ordinals. No prompt content is stored or matched.

`usage.json` is read-only: its write APIs are removed. Exact legacy annotations
remain readable; legacy marginal buckets never enter the Session options picker
or SQLite analytics.

## Exact option cohorts

`session:option_usage(option_id, callback)` snapshots the confirmed configuration
and resolves Provider for each advertised candidate with every other typed option
unchanged. It acknowledges pending recorder writes before querying committed
history. A candidate with unresolved Provider returns an attribution error; a
storage failure remains distinct from an empty cohort. Callbacks run on the main
loop. The picker discards pending results after configuration changes, Session
switching, a newer picker, or Disposal.

`store:usage_summaries(cohorts, callback)` is the shared typed SQL query boundary.
Each closed filter specifies Agent, Provider, and the complete option tuple. One
read-only transaction queries all candidates; it creates no store and migrates no
history. SQL excludes unsent attempts and turns with associated option changes.
No match returns zero turns and absent measurements, never a broader fallback.

Summaries return dispatched turn counts, outcome counts (including `unobserved`),
each reported token field's mean and measurement count, and complete cost deltas
with separate means and counts per currency. Cost coverage requires a terminal
peer response and the uninterrupted baseline/readings described above. Zero is
a measurement; absent telemetry is not. The picker names total turns and the
denominator for each displayed metric, or shows `No matching history`.

## Interactive usage history

`:LouiselmUsage` opens committed history without requiring a live Session. It
starts with totals across all recorded dispatched turns. There is no optional
configuration gate. Each refresh queries SQLite asynchronously; closing the
buffer or replacing the query cancels its reader. Pending recorder writes are
not flushed by exploration. Refresh after a turn finishes to include new facts.

| Key | Action |
| --- | --- |
| `f` | Edit joint filters as a JSON object; `{}` clears them |
| `g` | Edit grouping as a JSON array of dimension names; `[]` removes grouping |
| `t` | Edit the UTC range as `{"from":"2026-09-07T00:00:00Z","until_time":"2026-09-08T00:00:00Z"}`; `{}` clears it |
| `b` | Cycle no bucket, UTC hour, UTC day |
| `d` | Browse paged recorded dimension values; Enter applies the selected value |
| `v` | Cycle summaries, turns, and observations/transitions |
| `m` | Cycle including, excluding, or inspecting only mixed turns |
| Enter / Backspace | Drill into a group, then a turn; return to the previous query |
| `s` | From a turn, show its Session timeline, including transitions between turns |
| `n` / `p` | Next / previous page |
| `r` / `q` | Refresh / close |

Dimension names are `agent`, `provider`, `model`, `session` (the ACP Session ID),
and `option:<recorded option ID>`. For example,
`{"agent":"codex","model":"astra","option:reasoning":"medium","option:enabled":false}`
is one joint filter. Boolean `false` differs from string `"false"`; JSON `null`
matches an absent dimension. Filters always use recorded attribution and the
immutable starting tuple. Unknown query fields and invalid values are errors.
Dimension values reflect matching recorded turns, not current Agent settings.

Turn ranges and buckets use the recorded `prepared_at` timestamp. Event ranges
use `observed_at`. UTC is explicit throughout; ranges include their start and
exclude their end. Hour/day buckets follow UTC clock/calendar boundaries, with
no local-time or daylight-saving conversion. Opening a bucket retains any
narrower outer time range. Ordering within an observation stream uses sequence
numbers to break timestamp ties; ordering across streams does not imply causality.

Overall and Agent/Session totals retain mixed turns. A summary filtered or grouped
by Provider, Model, or options excludes mixed turns and reports the exclusion
count. This conservative rule never allocates a mixed turn's measurements to a
fixed configuration. Turn listings still support starting-value filters and
`mixed = "only"`, so excluded evidence remains inspectable. Details show the
starting tuple, cost baseline, recorded usage/outcome, changed-during-turn state,
option replacements, observed source, and request links only where established.
Between-turn transitions are available in the Session timeline; a configuration
filter cannot attribute those transitions to an unrecorded Provider.

Each token field and currency has its own sum, mean, and measured-turn coverage.
Unreported metrics remain absent; zero remains a measurement. Costs use the
picker's complete-delta rules above and never combine currencies or estimate
prices. The display bounds pages to 25 rows; the API permits 1–100 rows. Facts
are retained indefinitely. Each page is a fresh snapshot; concurrent recording
can change totals or page positions, so refresh to restart an inspection.

### Headless query API

The recorder extends the picker's query boundary with
`store:usage_query(query, callback) -> cancel`. It uses the same SQL metric
calculation as `usage_summaries`; Lua only submits queries and renders results.

```lua
local recording = require("louiselm.session.recording")
local paths = require("louiselm.paths")
local store, err = recording.new(vim.fs.joinpath(paths.state(), "usage"), function() end)
if not store then
  -- Handle err.code / err.message.
  return
end
local cancel = store:usage_query({
  view = "summary", -- also "turns", "events", or "dimensions"
  filters = { agent = "codex", ["option:enabled"] = false },
  group_by = { "provider", "model", "option:reasoning" },
  bucket = "day", -- "none", "hour", or "day"
  from = "2026-09-07T00:00:00Z", -- optional; inclusive
  until_time = "2026-09-08T00:00:00Z", -- optional; exclusive
  mixed = "include", -- or "exclude" / "only"
  limit = 25,
  offset = 0,
}, function(page, query_error)
  -- Main-loop callback: inspect page or handle the typed query_error.
  -- page.summary: turns, outcomes, tokens[field], costs[]
  -- Each metric: samples, average, total; cost entries also have currency.
  -- page.rows: grouped summaries / starting turns / events / dimension values.
  -- page.total, page.next_offset, page.mixed_turns, page.excluded_mixed.
end)
-- cancel() suppresses delivery and stops only this query's SQLite process.
```

`view = "events", turn_id = "<durable turn ID>"` returns that turn's observations
and linked transitions. Use Agent and `session` filters without `turn_id` for the
Session timeline. Missing storage produces an empty page without creating a
database. Invalid filters, corrupt metadata, permissions and query/storage
failures return typed sanitized errors. A cancelled query has no callback;
otherwise it completes once. The private reader keeps its bounded timeout.

Advisory reflection remains separate follow-up work.
