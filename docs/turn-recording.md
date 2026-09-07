# Durable turn recording

Every production Session, including headless Sessions, records turn metadata in
`stdpath("state")/louiselm/usage/turns.sqlite3`. There is no opt-out. Headless
owners may choose an absolute private directory with `Session.new`'s third
argument, `{ usage_directory = path }`.

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
ACP response supplies completion/failed/cancelled evidence; watchdog, transport
loss, and Disposal explicitly record that no terminal response was observed.

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
| `dispatch` | `request_id`: the locally accepted ACP write; no receipt claim |
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

New/legacy replay association and retirement of the old UI-owned `usage.json`
writes are `louiselm-lc32`.
Until that replacement lands, the legacy file still serves its existing UI
consumers; it is not imported into this database. Analytics queries, picker
cohorts, history views, and reflection are separate follow-up work.
