# louiselm-usage

A Rust/clap CLI for local, cross-project agent-history analytics. It ranks
recorded tools and commands and links them to Session IDs, historical
configuration where proved, and line/row evidence. JSON is the default contract;
no service, credentials, network access or transcript replay is needed at runtime.

## Install and run

Requires the repository Rust toolchain and system SQLite development headers
(`libsqlite3-dev` and `pkg-config` on Debian-family systems).

```sh
cargo install --path usage-cli --locked
louiselm-usage sources --discover --limit 5
louiselm-usage index --all
louiselm-usage stats commands --limit 10 --fields family,calls,retained_output_bytes,output_measured_calls
louiselm-usage stats tools --sort retained_output_bytes:desc --limit 10 --fields tool,calls,retained_output_bytes
```

Run installation from the repository root. Analysis then works from any CWD.
Only `index` changes the private derived database; source histories stay
read-only. Exit 4 means partial indexing, not an unusable database. Inspect
`sources --state partial` and source error states before interpreting results.

The default index is `$XDG_STATE_HOME/louiselm/usage-analysis/index.sqlite3`, or
`~/.local/state/louiselm/usage-analysis/index.sqlite3`. Use global `--db PATH` for
an isolated private index. Files are 0600, their directory 0700; an existing
nonprivate path is rejected. Incompatible schemas require a new `--db` path,
not an automatic migration or destructive rebuild.

## Query contract

```sh
louiselm-usage schema
louiselm-usage schema stats commands
louiselm-usage schema calls
louiselm-usage schema show call
louiselm-usage schema stats turns
louiselm-usage schema options turns --limit 5
louiselm-usage stats commands --project-tree /home/lotso/code/louiselm --group-by family,model --limit 10
louiselm-usage calls --family 'git status' --limit 3 --fields id,session_id,turn_id,provider,model,options,command_key
louiselm-usage show call CALL_ID
louiselm-usage stats turns --cohort fixed --group-by provider,model,option:OPTION_ID
louiselm-usage stats requests --group-by adapter,model
```

Replace `CALL_ID` and `OPTION_ID` with observed IDs. `schema stats SUBJECT`
lists that subject's default fields, metrics and group dimensions; `schema calls`
and `schema show KIND` describe their own projections. Schema descriptions work
without an index. Stats projections contain metrics plus selected group dimensions.

`schema options SUBJECT` discovers option IDs and JSON types from the existing
index for exactly one query subject: `calls`, `tools`, `commands`, `sessions`,
`turns` or `requests`. It follows that subject's default selection (leaf calls,
possible mirrors excluded; dispatched durable turns). `observations` counts
records with that ID/type; `missing_records` counts selected records without it.
Coverage adds `subject_records`, `options_complete_records` and
`mixed_option_records`; the other coverage fields still describe the whole index.
Discovery is unfiltered; narrower queries can have less coverage.

For `stats turns`, discover with `schema options turns`, then use the returned
ID in `--group-by option:OPTION_ID`. Call-native keys such as `codex.effort`
are not aliases for durable keys such as `reasoning_effort`. Native requests have
no proved historical-option join: discovery returns no options and explicitly
reports `options_supported=false`. Never reuse another subject's keys to fill
that gap. An all-null cohort remains unknown.
`--option 'KEY="high"'` and `--option 'KEY=true'` are distinct typed filters.

Filters cover project/tree, Session, adapter, configured Agent, Provider, Model,
source, recorded parent/child relationship, tool/family/exact command digest,
signature/wrapper, status, minimum output/duration and RFC3339 time bounds.
Same-field repeated values are OR; different fields are AND. `--since` is
inclusive and `--until` exclusive. `day`/`month` grouping uses UTC.

Group by up to eight dimensions. Sort by a selected group or metric. `--fields`
projects results before output; unknown fields fail even on empty selections.
Default 20 rows, maximum 1,000 requested rows and 32 KiB returned JSON. Follow
`next_cursor` with the same query/projection. A changed index generation invalidates
old cursors. `show` pages evidence with `--limit` and `--cursor`; `--fields`
limits its record. `--format table` offers a human-readable tab-separated view.

`sources` lists indexed metadata, freshness and diagnostics. Before an index
exists, or with `--discover`, it lists known filesystem sources without importing
them. `index --source FORMAT=PATH` adds an explicit file/archive directory.

Exit codes: 0 success, 2 invalid query, 3 I/O/index integrity failure,
4 partial indexing. Errors use JSON on stderr; results use stdout. Help/version
remain ordinary clap text. Queries use a consistent read transaction.

## Sources and accounting

| Format | Retained evidence |
| --- | --- |
| Codex | Rollouts, tool lifecycles, structured command/file/extension operations, native response usage |
| Claude | Tool-use/results, message usage, explicit child identity |
| OpenCode | Read-only SQLite snapshot of sessions, messages and mutable parts, including WAL updates |
| Copilot | Event tool lifecycle, historical model changes, message output usage |
| Gemini | JSON and append/replace JSONL journals; message/tool IDs merge updates |
| ACP adapter | `history.jsonl` plus identity metadata; current model/options are not historical evidence |
| ACP proxy | Prompt RPC/tool lifecycles; native aliases only when explicitly proved |
| LouiseLM | Dispatched durable turns, immutable options, option changes and reported usage |

Known roots are under the current user's configured state/data/harness homes;
discovery never recursively scans home. Internal symlinks and other owners'
files are skipped and reported. Schema changes and unsupported records are gaps,
not permission to invent fields or claim complete real-world activity.

Changed JSONL files are reparsed at file granularity (not byte checkpoints).
Records are limited to 32 MiB. Incomplete tails, malformed records and changing
sources are reported; other usable sources can commit. Mutable SQLite sources
are always read through a consistent read-only transaction. An atomic index
transaction publishes canonical calls after joining all available observations.
Unchanged facts are idempotent; missing sources retain disclosed stale snapshots.
Normalizer source changes invalidate cached JSONL classifications on the next
`index --all`, even when history contents are unchanged. Subsequent refreshes
skip them again. Compare identical call IDs and command digests to measure a
parser change; aggregate counts from growing histories are not a paired sample.

## Measurement rules

- Retained UTF-8 text bytes are not delivered, billed, avoidable or useful tokens.
  Produced bytes, when present, are separate. Shell `argument_bytes` is not a
  universal tool-input measurement. Structured/binary-only output stays unknown.
- Reported consumption in `stats turns` and native `stats requests` overlaps:
  never add their totals. Native requests group by message/response scope and
  basis. Gemini's recorder-defaulted zeros are distinguished. Legacy Codex
  cumulative snapshots are excluded from additive totals and reported as gaps.
- Null means unknown, not zero. Each sum/mean has observed denominators. Cache
  and reasoning categories can overlap input/output/total; do not add blindly.
- Default leaf rankings exclude outer orchestration; `--level all` separates
  levels. Native IDs deduplicate replay and proved mirrors. Possible mirrors
  are excluded unless `--overlap include`; conflicts stay visible. Coverage is
  whole-index; row denominators are group-specific.
- Historical configuration uses unique explicit identity joins, not timestamps,
  current settings, or Provider guesses. Reused RPC IDs remain ambiguous.
  `--cohort fixed` excludes incomplete/mixed options. Observational cohorts do
  not control task difficulty or establish causal savings.
- Command families identify the first literal executable or shell builtin,
  including recognized project scripts. Quoted names, leading assignments and
  comments are understood. Known `env`, `sudo`, `command`, `exec`, `timeout`
  and RTK wrappers can compose; `wrapper` records the outermost recognized one.
  Option operands are consumed; lookup modes and unsupported options retain
  the wrapper's own family. Dynamic executable expressions remain opaque.
- Simple signatures redact operands; complex arguments preserve only the known
  prefix. A pipeline or `cd DIR && git status` is one recorded shell operation:
  only its leading command (`cd` here) is classified. Later stages are not counted
  or claimed to have executed. Unlisted executable names remain unknown; paths
  and arbitrary names are never copied into signatures. Exact private SHA-256
  equality keys are not anonymization.

The database stores no raw prompts, tool results, patches, or command operands.
Paths, metadata and settings remain sensitive. Source content is never executed
or included in diagnostics. Evidence points to indexed line/row locations and
source-generation digests; rotated history can invalidate a live pointer.

## Skill and scope

The owned skill source lives in the maintainer's dotfiles catalog at
`agentskills/.config/agentskills/louiselm-usage/`, not in its generated installation.
Its short instructions use this prebuilt executable; no new Python/shell helper
scripts are required. Catalog rebuild scripts are existing installation tooling.

The [design](../docs/session-usage-skill.md) includes the wider useful-statistics
catalog. This first version deliberately leaves raw-content inspection, token
estimators, percentiles/cost conversion and richer shell normalization for later.
Hooks, command rewriting, remote collection, daemon/UI and automatic optimization
are outside this feature. Use results to choose a bounded follow-up experiment.

## Verification

```sh
cargo fmt --manifest-path usage-cli/Cargo.toml --check
cargo clippy --manifest-path usage-cli/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path usage-cli/Cargo.toml --all-features --locked
RUSTDOCFLAGS=-Dwarnings cargo doc --manifest-path usage-cli/Cargo.toml --no-deps --all-features --locked
```

Run these from the repository root; CI runs the same gates. Fixtures preserve
observed local/installed-official format structure with synthetic IDs/content.
Tests use private temporary stores and never read actual user histories.

The initial local acceptance corpus had 1,590 readable sources, about 122,000
canonical calls, and 17,449 native usage records. Cold import plus reconciliation
took 11.26 seconds (41 MB peak RSS); a projected ranked query took 0.91 seconds
(11 MB). These are one-machine observations, not a performance guarantee.
The later structured-file/extension importer brought the indexed call count to
about 126,200. Full refreshes during this live session changed two active sources
and took 6.50–6.59 seconds; a genuinely unchanged 246-source Claude refresh took
0.38 seconds (7 MB), with no generation change. Do not present the live refresh
as a no-change benchmark.
