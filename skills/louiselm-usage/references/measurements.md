# Measurement and comparison rules

## Boundaries

The index is metadata, digests, measurements and pointers, not a transcript copy.
It stores no raw prompts, tool results, diffs, or shell operand values. Metadata
still contains sensitive paths, IDs and historical settings: keep the index
private and local. SHA-256 equality keys are not anonymization or encryption.

`retained_output_bytes` measures UTF-8 text present in the selected record. It
does not prove how much reached a model, was read, was billed, or was useful.
`produced_output_bytes` is separate and populated only when explicitly recorded.
`argument_bytes` currently measures recorded shell-program text, not every tool
argument. Binary and structured-only outputs have missing text measurements.

Totals and means include only observed values; their `*_measured_*` denominators
must accompany comparisons. Null is unavailable, not zero. Exit-code coverage
differs from reported tool success. Tool success does not establish task success.
Duration sums are observed operation time, not elapsed Session time; concurrent
operations overlap. Truncation remains unknown without an explicit flag.

## Consumption versus context

`stats turns` uses dispatched LouiseLM durable turns and recorded usage fields.
`stats requests` uses deduplicated native response/message records. Those scopes
overlap: never sum them together. Request grouping always includes `scope` and
`basis`, because a message is not necessarily a provider request.

`basis=recorder_defaulted` means the native recorder may replace missing upstream
fields with zero (Gemini). Do not label those zeros proven provider measurements.
Older Codex cumulative snapshots are explicitly excluded from additive totals.
Cache counters may already be included in input/total tokens. Reasoning and tool
token categories also differ by provider. Never sum all categories blindly.
Context occupancy and quota counters are not consumption. There are no billing
estimates, price lookups, or per-tool billed-token allocations in this version.

## Identity and cohorts

Calls retain adapter-native identity and Session IDs. Replayed updates merge by
native ID; proved ACP mirrors merge with all available evidence references.
Native evidence is the deterministic representative when both native and ACP
observations exist. Differing command/output fingerprints flag a conflict; this
can reflect different output boundaries, not necessarily a corrupt source.

Possible unproved mirrors are excluded from default rankings. Their facts remain
available with `--overlap include`; including them does not make sets disjoint.
Coverage counts describe the whole index; row denominators describe each group.
Unknown mirrors across unrelated identifiers cannot be detected automatically.

Historical Agent/Provider/Model/options join only through explicitly recorded,
unique identities. Reused JSON-RPC IDs on resumed sessions stay ambiguous. Native
turn UUIDs are not LouiseLM durable turn IDs. Route identifiers are exposed as
`provider_route`, not promoted to Provider. Mutable current metadata is never
backfilled as historical model/options.

`--cohort fixed` excludes incomplete option tuples and turns with any observed
option change, even a change back to the initial value. Check
`mixed_turns_excluded`. Absence of a recorded change is not proof that every
upstream change was observable. Task mix, project, model and time can confound
comparisons. These are descriptive cohorts, not causal evaluations.

Discover fields with `schema stats turns` and actual option IDs/types with
`schema options turns --limit 5` before constructing durable cohorts. Follow
`next_cursor` when needed. `schema options calls` instead describes canonical
call settings; similarly named keys are not interchangeable. Discovery counts
use default subject selection, not the narrower filters of a later query.
`subject_records` is the denominator; each ID/type has `observations` and
`missing_records`. Completeness and mixed-option counts describe the whole
selected tuple, not proof that any particular key was recorded. Request usage
explicitly lacks a historical-option join; do not fill that gap from calls.

## Command normalization

`command_key` is a private equality digest of exact recorded shell text.
`simple` signatures retain known executable/subcommand names and flag names;
operand and attached option values are redacted. `prefix_only` preserves a known
unquoted program prefix while leaving complex arguments opaque. It does not
enumerate shell children or prove that a later pipeline stage executed.
Unknown executables and opaque scripts stay visible as such. No shell program is
evaluated to determine its meaning. Group by `normalization` to quantify gaps.

## Useful experiments, not promised savings

```sh
louiselm-usage stats commands --sort retained_output_bytes:desc --limit 10 --fields family,calls,retained_output_bytes,output_measured_calls,output_missing_calls
louiselm-usage stats commands --group-by family,command_key --sort repeat_output_calls:desc --limit 10 --fields family,command_key,calls,repeat_output_calls,output_measured_calls
louiselm-usage stats tools --group-by adapter,tool --sort failed_calls:desc --limit 10 --fields adapter,tool,calls,failed_calls,exit_measured_calls,nonzero_exits
louiselm-usage stats commands --group-by day,family --family 'git status' --limit 20 --fields day,family,calls,retained_output_bytes
```

High frequency suggests recurring local work; high text volume suggests bounded
reads, field projection, or local aggregation. Repeated fingerprints suggest
checking whether a reusable local result would help, but repeated outputs can be
necessary confirmations. RTK-style compression is a possible future experiment,
not an action this skill installs or a measured reduction in provider billing.

## Refresh and recovery

Discovery covers known current-user stores for Codex, Claude, OpenCode, Copilot,
Gemini, ACP adapter/proxy and LouiseLM. It does not scan arbitrary home folders,
other users, deleted histories or remote machines. Add an explicit local archive
with `index --source FORMAT=PATH`; `schema` lists format identifiers.

Refresh reparses changed files transactionally; unchanged JSONL files are hashed
and skipped. Mutable SQLite stores use read-only snapshots. Malformed records,
incomplete tails, unsupported items and unavailable files produce visible gaps.
Queries do not refresh, repair, replay, or open source payloads. Vanished sources
retain their last snapshot with unavailable status. `show` pointers can therefore
refer to changed or missing history; their digest names the indexed generation.

The default index is `$XDG_STATE_HOME/louiselm/usage-analysis/index.sqlite3`, or
`~/.local/state/louiselm/usage-analysis/index.sqlite3`. `--db` selects an isolated
index. Rebuild an incompatible index at a new private path; do not delete or
overwrite source stores. Exit codes: 0 success, 2 query error, 3 I/O/index failure,
4 partial indexing. A returned cursor is for exactly that query and generation.

This first version deliberately has no raw-content drill-down, token estimator,
cost conversion, arbitrary SQL, daemon, or hooks. Use source pointers for a
separately authorized, bounded inspection; never dump a transcript to work around
an unavailable measurement.
