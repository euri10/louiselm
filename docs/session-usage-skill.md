# Cross-project session usage skill

Accepted design, 2026-09-17. Skill and executable: `louiselm-usage`.
The first implementation is in [`usage-cli/`](../usage-cli/README.md); its README
and `schema` describe the shipped interfaces. The owned skill is maintained in
the dotfiles skill catalog, with details loaded progressively.

Tracked as P2 feature `louiselm-session-usage-skill-hq203`. Automated gates and
local-corpus checks are recorded in its delivery tasks; final maintainer
invocation/acceptance remains separate from those checks.

Implementation choices: file-granular refresh, atomic publication of canonical
facts, private metadata-only storage, bounded JSON plus optional table output.
Raw-content drill-down, optional token estimation, percentiles/cost conversion,
richer shell normalization and broader origin metadata remain follow-up work;
the relevant sections below describe design intent, not shipped claims.
Those extensions are tracked as `louiselm-7p7n0`; safe opt-in content inspection
is tracked separately as `louiselm-ix2bd`.

## Outcome and boundary

Ask which tools and commands recur across the operator's local agent histories,
which return the most output, and where a local operation could reduce repeated
context. Every aggregate must lead back to its contributing Sessions, turns,
calls, historical options, and source records.

Default scope is all recognized histories belonging to this OS user, across
projects. Discover known history locations and explicit additional roots; do not
recursively scan the home directory. Include recorded child sessions with their
parent relationship, and support root-only filtering. Ownership of a history
directory establishes scope, not proof that a human personally initiated every
turn. Preserve known automation/test origins; leave unrecorded origins unknown.
Other machines and deleted histories require operator-supplied local archives.

This is retrospective analysis. Hooks, command rewriting, automatically changing
Agent options, replaying commands, cloud collection, and a Neovim dashboard are
outside this feature. Recommendations are advisory and require evidence. Tool
success is not evidence that the surrounding task succeeded.

Use the canonical Beads vocabulary for Agent, Model, Provider, Session, and AX.
External histories can have an adapter and native conversation ID without a
known configured LouiseLM Agent. Do not invent an Agent to fill that gap. This
cross-session index is separate from the single-subject Forensics record.

## Existing work and evidence

[Durable turn recording](turn-recording.md) already supplies immutable attribution,
typed option snapshots, option changes, outcomes, and reported consumption.
Consume its committed database read-only; preserve its semantics and storage
boundary. The new index must not add tool content to `usage/turns.sqlite3`.

Related Beads work:

- `louiselm-rdwb`: collect usage, expose factual queries, then reflect.
- `louiselm-a1a9`: advisory interpretation of option-change patterns. Its existing
  metadata-only scope remains intact; this proposal explicitly designs a
  separate local transcript-analysis capability.
- `louiselm-fbcs`: the Neovim history explorer; this CLI does not require its UI.
- `louiselm-9ytn`: query-host measurements. This CLI has an independent consumer:
  cross-project native histories outside Neovim, plus the user's Rust requirement.
  It does not replace or accelerate the existing Lua query host by assumption.

The following observations came from local file inventories, selected record
shapes, CLI help, and read-only SQLite queries. Counts are a discovery snapshot,
not a deduplicated session census or an implemented support claim.

| Input | Observed local evidence | Intended contribution |
| --- | --- | --- |
| Codex | 836 rollout files; session metadata, turn context, tool records, usage records, and completed execution events | Native sessions, calls, command executions, Model/options, reported usage |
| Claude | 246 JSONL files including child histories; assistant tool blocks and usage, linked tool results | Native sessions, parent relationships, calls, command arguments, reported usage |
| OpenCode | 57 sessions, 904 messages, 3,674 parts in its SQLite database | Session/message/part identities, structured tools and usage after fixture verification |
| Copilot | 89 `events.jsonl` files; explicit tool start/complete events, Model changes, turn IDs | Calls and configuration history; only usage fields actually recorded |
| Gemini | Two sampled chat journals use an initial record followed by `$set` updates | Reconstruct recorded state before extracting facts; do not count each patch as new work |
| ACP adapter | 174 `sessions/*/history.jsonl` files and 144 connection logs | Additional native histories and explicitly linked construction/configuration evidence |
| ACP proxy | 22 session logs plus connection logs under `proxy/` | Correlation, lifecycle/configuration enrichment, fallback where native history is absent |
| LouiseLM | `usage/turns.sqlite3` contains multiple Agents and Providers | Authoritative LouiseLM turn identity, Provider, options, and reported turn usage |

Discover current roots under `~/.codex/`, `~/.claude/projects/`,
`$XDG_DATA_HOME/opencode/`, `~/.copilot/session-state/`, `~/.gemini/tmp/`, and
`$XDG_STATE_HOME/acp-llm-adapter/`, respecting documented application overrides.
Codex archived histories and explicit imported roots belong in discovery too.
Do not inspect credential files alongside histories. Both ACP adapter histories
and the nested proxy logs exist here; assuming only `proxy/sessions/` loses data.
Construction-time ACP frames can be in connection logs before a Session is bound.

In the inspected Codex session, outer tool calls are `exec` with JavaScript input,
but separate `item_completed` / `CommandExecution` events contain argv, cwd,
duration, exit status, raw output and formatted output. Import observed execution
events instead of guessing executed commands from JavaScript text. Other formats
need their own evidence-backed extraction rules.

RTK already compresses output and exposes `gain` and Claude-history `discover`.
Its upstream documentation describes token estimates based on bytes divided by
four, distinct from bill savings. Reuse this distinction; an installed RTK
history can later supply separately identified before/after measurements, but
is not required for importing unwrapped commands. See [RTK's savings explanation](https://github.com/rtk-ai/rtk#how-savings-work).

## System and ownership

```text
Recognized native histories + ACP evidence + LouiseLM turn database
                         |
                  explicit local index
                         |
       private SQLite facts, measurements, source pointers
                         |
          bounded CLI queries -> skill -> cited analysis
```

Use a standalone Rust crate, proposed location `usage-cli/`, with Clap subcommands.
Keep it independent of Neovim, the capture service, and trusted Skill Admission
machinery in `skills-core`. A synchronous, streaming batch CLI suffices: no daemon,
watcher, async runtime, adapter plugin registry, or generic event framework.

Separate concrete parsers, normalization, SQLite storage/querying, and CLI
presentation into cohesive modules. Use SQL for grouping/filtering and Rust for
extraction and orchestration. Proposed runtime libraries are Clap, Serde/JSON,
rusqlite, and only the time/digest support the implementation actually needs.
This design adds no dependencies; implementation must justify additions and
follow the repository's Rust/development-dependency policy.

The owned derived index lives at
`$XDG_STATE_HOME/louiselm/usage-analysis/index.sqlite3` with the usual state-home
fallback. Source databases and logs remain read-only. The derived database is
rebuildable, with a schema version and parser/normalizer versions. Query commands
never import, migrate, rebuild, or repair implicitly.

## Facts, identity, and joins

Use a small relational model with JSON for typed option tuples:

| Record | Essential fields |
| --- | --- |
| Source | Stable source ID, format, origin, locator, file generation/checkpoint, parser version, observed coverage/errors |
| Session | Namespaced native ID, optional LouiseLM identity, adapter, known parent/root, observed project/cwd/origin |
| Turn | Native and/or durable turn IDs, historical Agent/Provider/Model/options, dispatch/outcome, mixed-options flag |
| Model request | Native request/response ID and reported token components with scope and cumulative/delta semantics |
| Call | Stable ID, native IDs, optional turn/parent call, namespaced tool name/kind, requested/started/terminal observations, timestamps/status |
| Command | Owning execution/call, exact-input fingerprint, sanitized signature, shell/executable/subcommand, known wrappers, normalization status |
| Evidence link | Fact ID, source ID, byte/record/row locator, source generation and record digest; multiple observations may support one fact |

Measurements on their owning records carry unit, scope, method, availability and
source. Avoid an untyped universal key/value event store. A native call can exist
without a proven turn association; unknown joins must not remove it from counts.

Identity and aggregation invariants:

1. Re-indexing the same evidence produces the same facts and IDs. Scope native
   IDs by source format and established session identity; filenames alone are
   not identity. Imports from another origin preserve that origin until an
   explicit recorded identity establishes equivalence.
2. Join native histories, ACP, and LouiseLM only through recorded identities or
   verified format-specific mappings. Timestamp proximity, shared cwd, similar
   prompts, and matching bare UUIDs from unrelated namespaces are not joins.
3. Count a tool lifecycle once, even if it has many updates, replayed records,
   or both native and ACP representations. Prefer native execution evidence for
   command detail and LouiseLM facts for its turn attribution. Conflicts remain
   visible; never overwrite one source silently with another.
4. An orchestration tool and its observed child executions are distinct levels.
   Provide `--level all|leaf|orchestrator`; tool rankings default to leaf calls.
   Count commands only from execution/tool-argument evidence, never prose.
   Unobserved nested calls remain a reported coverage gap.
   An execution record without a linked tool call still supports command stats;
   do not invent its tool name, parent call or call-to-command association.
   All-level reports keep output subtotals separate so wrapper output and its
   child results cannot be summed into a misleading combined total.
5. Repeated launches are distinct executions. Polls and continuation output attach
   to the same execution when a recorded handle proves the relationship. A shell
   invocation is one execution; syntactic pipeline/conditional components are
   occurrences, not independently proven executed processes.
6. Do not aggregate overlapping source sets as if they were disjoint. Proven
   duplicates are merged with their evidence links; unresolved overlap gets
   separate totals and a diagnostic. Retain unknown Provider/Model buckets.
7. Child histories and copied ancestor context must not inflate counts: use
   recorded parent/fork links and event IDs. Equal content alone never proves
   that two actual executions were the same execution.

Keep both exact-input equality and conservative command families. Preserve flag
names and meaningful subcommands; redact operand values in normal responses.
Recognize recorded argv and explicit wrappers such as `rtk`/`rtk proxy`. Preserve
the wrapper chain for adoption comparisons. Do not normalize `git status` and
`git diff` together or remove flags that change output semantics.

The first implementation must always index an opaque command with a fingerprint
when family extraction is unsupported. Family grouping reports its coverage.
Do not invent a shell parser using whitespace splits; quoted strings,
assignments, pipelines, heredocs, substitutions and nested shells require a
supported parser or an explicit opaque result. Never execute input to classify
it, and never evaluate JavaScript wrapper code from history.

## Measurement rules

Keep three kinds of quantity separate:

- **Recorded output size:** UTF-8 bytes/lines of available argument and result
  text, with the boundary named: produced, retained, or delivered to the model.
  A log's JSON serialization size is not the result text size. Formatting,
  truncation, output chunking and wrapper envelopes matter. Where actual model
  delivery is not evidenced, call it retained output, not delivered output.
- **Estimated text tokens:** optional calculation over a named representation,
  with estimator/tokenizer name and version. Bytes/4 is a rough proxy, never
  provider-reported usage. Unsupported tokenizers and nontext content stay
  unavailable; do not estimate image/audio costs from JSON or base64 length.
- **Reported consumption:** original provider/adapter fields at their recorded
  request/turn/session scope. Keep input, output, reasoning, cache reads/writes
  and reported totals separate; a total may already include components.

Deduplicate streaming/repeated request records by recorded IDs. Never sum
cumulative session counters as per-request usage; reset/resume boundaries and
decreases invalidate an unproven delta. Prefer a reported turn total over a
derived request sum for that turn, and show which method was selected. Validate
against the existing LouiseLM recorder contract, including absent measurements,
unknown completion and currency-separated complete cost deltas.

Never divide turn input tokens among tools to invent their cost. Tool outputs can
reappear in later input, be cached, summarized, compacted, truncated or omitted.
Historical output reduction therefore does not establish billing, quota or
end-to-end token savings. Do not multiply an output estimate by later turn count.

Every aggregate names its eligible population, measured count, missing count,
exclusions and units. Null means unavailable; zero means measured zero. Durations
distinguish execution, delivery and orchestration wait when recorded; a timestamp
gap is not CPU time. Overlapping durations do not add to wall-clock time.
File modification time is not an event timestamp. Untimed records remain in
all-time counts, with explicit exclusions from time-filtered populations.

Options are historical and typed. Keep the complete known option tuple plus
actual Model/Provider observations at their recorded boundaries. In fixed-option
comparisons, exclude turns with any associated option change, including a change
that returns to its initial value. Never allocate that turn's tokens across its
configurations. Missing options are not defaults. Unknown option coverage cannot
support a claim that every other option was held fixed.

## Agent-facing CLI

Canonical commands are `sources`, `index`, `stats`, `calls`, `show`, and `schema`.
`show` supports call, Session and turn IDs. JSON is the default; an explicit
`--format table` is for human inspection. The following are proposed interfaces,
not commands available in the repository today:

```sh
louiselm-usage sources
louiselm-usage index --all
louiselm-usage stats tools --sort calls:desc --limit 20
louiselm-usage stats commands --sort retained_output_bytes:desc --limit 20
louiselm-usage stats commands --project /home/lotso/code/louiselm \
  --group-by family,provider,model --since 2026-09-01T00:00:00Z
louiselm-usage stats turns --group-by 'provider,model,option:<OPTION_ID>' \
  --cohort fixed --sort reported_input_tokens:desc
louiselm-usage calls --command-key <key> \
  --fields id,session_id,turn_id,provider,model,options,evidence --limit 10
louiselm-usage show call <id>
louiselm-usage schema stats
```

`sources` reports discovered, indexed, inaccessible, missing and unsupported
inputs, last refresh, time coverage and gaps. `index --all` refreshes recognized
roots; repeated `--source FORMAT=PATH` adds explicit local roots. First-run
queries return `index_missing` with the exact next command; they do not scan raw
logs behind a seemingly cheap query.

Use validated, composable flags across applicable queries:

| Dimension | Filters/grouping |
| --- | --- |
| Scope | Source/format, local origin, exact project/cwd, explicit project subtree, root/child/automation origin |
| Identity | Session, parent/root Session, turn, call and native tool/server IDs |
| Time | Inclusive `--since`, exclusive `--until`, explicit UTC instants; hour/day/week buckets with a declared timezone |
| Configuration | Agent, adapter, Provider, Model, version, typed `--option KEY=JSON_VALUE`, full option tuple, fixed/mixed/unknown configuration |
| Operation | Raw tool name, normalized kind, command key/family, executable/subcommand, known flags, wrapper, supported/opaque normalization |
| Result | Observed status, exit code, error category, truncation, measured duration/size ranges and measurement availability |
| Presentation | Repeated grouping, validated metrics, stable sort/tie-break, field projection, row limit and continuation cursor |

Agent and Provider identifiers remain distinct even when their spelling matches.
Native setting names remain namespaced; expose a shared dimension only when its
meaning is established. An unobserved Git branch/commit or requested option is
unavailable, not reconstructed from today's checkout/configuration.
Replace `<OPTION_ID>` with an option identifier returned by `schema`. Repeated
values for one ordinary filter are ORed; different filters are ANDed. Distinct
`--option` keys are ANDed; contradictory repetitions of one key are invalid.

Defaults: 20 rows and a 32-KiB response ceiling. Return complete valid JSON within
the ceiling, with `schema_version`, index generation, applied scope, rows,
coverage, diagnostics and `next_cursor`. Paginate on stable IDs within an index
generation; reject a cursor for a replaced generation. No silently truncated JSON
or undocumented sample. Project/group predicates run in SQLite before rendering.

`schema` returns valid subjects, fields, metric units, filters and operators for
one command, so an Agent need not load a manual. Unknown flags, fields or typed
values are errors with a small allowed-value hint. No arbitrary writable SQL or
general query language in the initial CLI.

Exit codes: 0 success (including a correctly described empty result), 2 invalid
query/input, 3 source/index I/O or integrity failure, 4 partial indexing. Partial
indexing commits usable sources and reports exact gaps; read queries still return
their coverage. Fatal errors are structured on stderr; results on stdout.
Noninteractive commands never prompt or install dependencies.

## Useful statistics and what they support

| Statistic | Evidence and useful interpretation |
| --- | --- |
| Calls by tool/command; distinct Sessions/projects; calls per dispatched turn | Establish frequently used operations, without equating frequency with waste |
| Total output bytes, share of measured output, mean/p50/p95/max | Find large context contributors; compare like measurement boundaries |
| Argument bytes versus result bytes | Locate large generated scripts/tool payloads as well as noisy results |
| Same command repetitions and identical output fingerprints | Identify candidates for caching, reuse or more selective reads; repetition alone is not redundant work |
| Repeated reads/searches and overlapping recorded ranges | Suggest indexing, bounded file reads or focused queries where targets/ranges are recorded |
| Exit codes, typed failures and later repeated invocations | Locate error/retry candidates; nonzero exit is not universally failure (for example search with no matches) |
| Truncation incidence and subsequent retrievals | Identify oversize queries and paging needs; lost bytes only when the original size is known |
| Execution duration distributions, pending calls, polls and orchestration waits | Find expensive interaction patterns without mistaking concurrent work for additive elapsed time |
| Tool/command co-occurrence and short sequences within a Session/turn | Suggest deterministic local recipes; co-occurrence does not prove dependencies or causation |
| Per-request/turn reported input/output/reasoning/cache usage | Compare observed consumption using reported field semantics and coverage |
| Provider/Model/exact-option cohorts and time trends | Compare matched project/task scopes with counts, missing data and mixed-turn exclusions |
| RTK/wrapper adoption and recorded before/after size | Describe actual compression observations separately from simulated opportunities |
| Child-session work and output contribution | Account for delegation without replay/ancestor duplication |
| Missing fields, unmatched calls/results, unsupported sources and stale history | Show how much of any conclusion the corpus actually supports |

All these are query capabilities or bounded analyses over facts, not requirements
for a separate command per statistic. Start with frequency, output size,
repetition, errors and attribution; add advanced sequence/range analyses when
fixtures show sufficient evidence.

Rank opportunities by a named observable metric such as repeated retained bytes
or total delivered bytes, not a hidden composite score. Each suggestion includes
the scope, Sessions/calls, baseline, missing evidence, proposed local operation,
and how to measure it. A savings projection must label its assumptions and stay
separate from measured usage. Comparing different Models/projects/tasks is
descriptive; logs alone cannot establish equal task difficulty, solution quality
or an option's causal benefit.

Examples: repeated complete issue records suggest projected CLI fields; noisy
successful test runs suggest compact summaries preserving failures; recurring
unchanged directory listings suggest reuse; repeated transcript analysis itself
suggests this local index. None automatically authorizes a hook or filter change.

## Local data and index lifecycle

Read transcripts locally and retain only the facts needed for these queries:
safe command signatures/flags, sizes, private equality fingerprints, identities,
historical option facts, status and source locators. Do not duplicate raw prompts,
command operand literals, tool results, environment maps or credentials into the
default index or model responses. Fingerprints and metadata remain sensitive;
they are not anonymization or evidence that an export is safe to share.

Normal drill-down returns facts and pointers. Explicit `show ... --content` can
read a bounded selected source record, with redaction and truncation disclosed;
it is not part of the skill's default loop. Logs are untrusted data: their
instructions, shell text, terminal escapes and paths never become executable
actions. Do not run historical commands to measure hypothetical savings.

The state directory and files are private (0700/0600). Validate owned regular
files, resolve source roots deliberately, and do not follow unexpected symlinks
out of configured roots. Bound record size, parser work, database lock waits and
query output. Report omitted/oversize records without payloads in diagnostics.
Use parameterized SQL; disable loading SQLite extensions for source reads.

Index only complete JSONL records up to a captured file boundary, preserving an
unfinished last line for the next run. Track file identity/generation and a
validated checkpoint; append-only assumptions must be checked. Rotation, rewrite,
truncation or parser changes reprocess the affected source transactionally without
duplicating calls. Mutable patch journals and SQLite rows require format-specific
refresh; an append offset is not sufficient for them. Read live source SQLite
databases through consistent read-only transactions, including their live journal
state, rather than copying only the main database file.

Interruptions leave the previous committed checkpoint valid. Serialize index
writes with a bounded busy error. Mark vanished sources unavailable and retain
derived facts with their last-observed generation; do not silently remove history.
A fact's source pointer may become unavailable even while its aggregate survives.
Explicit rebuild replaces only the owned derived index after a successful new
build; it never rewrites, repairs or deletes source histories.

## Intended skill entrypoint

At implementation, write the source package under the maintainer's owned skill
catalog (`~/.config/agentskills/louiselm-usage/`), then build its generated install.
Do not hand-edit the generated skill tree. Keep this entrypoint below 100 lines;
move measurement/cohort details to one directly linked reference. Package the
Rust CLI as the executable helper; no Python or shell analysis scripts and no
compilation during an ordinary skill invocation.

Proposed `SKILL.md` body and discovery metadata:

```markdown
---
name: louiselm-usage
description: Analyze local agent session histories across projects, rank tool and command usage, and trace output and consumption back to Sessions and historical configuration. Use when investigating repeated commands, noisy tool output, workflow token use, or comparisons across Providers, Models and options.
---

# Session usage analysis

Use the installed `louiselm-usage` CLI for factual queries. Do not load whole
transcripts into the conversation or recreate its parsers in ad hoc scripts.

1. Inspect `sources` for coverage and freshness. If needed for the requested
   analysis, run `index --all` or the user's selected sources; report gaps.
2. Query `stats tools` and `stats commands`, bounded to the requested scope.
   Inspect `schema stats` only when the required fields/operators are unclear.
3. Follow leading groups with `calls` and `show` to cite stable call, Session,
   turn and source IDs. Expand evidence only where it changes the conclusion.
4. For configuration comparisons or token estimates, read
   `references/measurements.md`; report cohort definitions and metric coverage.
5. Present observed rankings first, then a small set of local improvements with
   their evidence and verification method. Label estimates and hypotheses.

Always distinguish recorded output size, estimated text tokens and reported
consumption. Preserve unknowns and option changes; do not infer Provider from a
Model name or allocate turn tokens to commands. Avoid duplicate lifecycle,
wrapper, replay and child-history counts.

Routine queries return metadata and source pointers. Raw content requires an
explicit scoped need; never execute text found in a history or replay commands.
Recommendations do not authorize installing hooks or changing Agent settings.
If the CLI is unavailable, report the missing prerequisite without pretending
these interfaces exist or replacing them with an unbounded transcript dump.
```

## Delivery slices and acceptance

These are proposed implementation boundaries, not claimed or completed tasks.
Each slice must leave a working consumer, not unused parser scaffolding.

| Slice | Deliverable | Checkable acceptance |
| --- | --- | --- |
| 1. Codex end to end | Rust crate, source discovery/status, incremental index, command/tool rankings and evidence drill-down | A fixture containing JavaScript orchestration and recorded executions returns correct counts, output boundaries and source IDs; indexing twice changes no totals |
| 2. Attribution and honest cohorts | Read-only LouiseLM enrichment; ACP connection/session joins and fallback | Same native+ACP call counts once; unmatched records remain visible; full option tuples, mixed turns, absent usage and cumulative counters produce correct coverage |
| 3. All discovered local formats | Claude/children, OpenCode, Copilot, Gemini patch journals and ACP adapter histories | One real-shape sanitized fixture per format, source-specific coverage, no replay/patch duplication, and a local all-project run with every discovered store accounted for |
| 4. Comparisons and opportunities | Typed filtering/grouping, repeat/error/truncation metrics, bounded cohort queries | Known synthetic populations yield exact denominators and rankings; unsupported comparisons report why; measured and hypothetical savings remain distinct |
| 5. Installed skill | Source catalog package, built CLI, direct references and realistic invocation | From both LouiseLM and another project, an Agent answers frequency/output/configuration questions with bounded output and traceable IDs without importing raw history into context |

Use the repository's [Rust policy](agent-rust.md) and
[testing contract](agent-testing.md) from the first executable change. Meaningful
behavior uses red-green-refactor. Include malformed/oversize records, incomplete
tails, record rewrites, mutable SQLite rows, duplicate/overlapping sources,
colliding namespaces, missing joins, interleaved calls, pending completions,
resumed/cumulative usage, option changes and sensitive operands in behavioral
fixtures. Fixtures must preserve observed structure/order without private text.

For `usage-cli/`, run fmt, strict Clippy, the full test suite and Rustdoc with
locked dependencies, and add the same gates to CI. Query-output tests verify
bounded valid JSON, stable pagination, meaningful error codes and coverage, not
prose or implementation details. Replay-free offline tests need no credentials,
network, benchmark framework or new development dependencies.

Measure initial index time/peak memory, a no-change refresh, and warm ranked
queries against the actual local corpus. Record dataset shape, command and
environment; performance numbers are not known yet. Acceptance requires a
maintainer invocation that traces a ranked command to its real Session and
compares an evidenced historical option cohort.

AX review applied: one explicit refresh boundary; one derived data store; narrow
discoverable queries; stable IDs and source references; bounded output; visible
uncertainty, gaps and freshness; no implicit repairs or source mutations. The
initial defaults are all local projects, included recorded children, metadata
output and unknown-safe cohorts. They can be revised during design review without
committing to hooks, a UI or automatic optimization.
