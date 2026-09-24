# louiselm.nvim Development Contract

## Project Scope

louiselm.nvim is an early-stage Neovim plugin with no external users. Breaking
changes are acceptable. Change the code directly; do not add compatibility
shims, deprecated aliases, or migration paths unless explicitly requested.

Priority: **correctness (including safety and security) → clarity → simplicity →
performance**.

Fix root causes at the narrowest shared boundary. Prefer deletion, existing
code, Lua/Neovim facilities, and plain functions over new abstractions. Do not
build extension points for hypothetical consumers. The established extension
boundaries are the headless session API, typed events, permission policies, and
agent configuration.

Permission prompting depends on both the configured Agent's upstream settings
and LouiseLM's permission policy. Preserve operator-selected YOLO/auto-approval
as a supported choice. Approval-UI QA may temporarily enable requests for the
tested Agent; it must not make human prompts mandatory for every Agent.

## Required references

This file is the always-loaded contract. The following repository documents are
also binding; read the applicable document before that work, once per session.
A complete, current copy already in context needs no second read. Recover only
missing/truncated sections, and re-read when the file changes.

| Before doing this | Read |
| --- | --- |
| Any tracker mutation, claim/liveness decision, or live-instance diagnosis | [Agent workflow](docs/agent-workflow.md) |
| Lua/Neovim implementation, design, review, dependencies, types, or public API docs | [Lua policy](docs/agent-lua.md) |
| Rust code, manifests, API docs, design, or review in any crate | [Rust policy](docs/agent-rust.md) |
| Executable changes, test/reliability design or review, gates, or CI monitoring | [Testing and acceptance](docs/agent-testing.md) |
| Android work | [Android contract](android/AGENTS.md) |

The root file is limited to 12,000 bytes by `./scripts/check-agent-instructions`,
enforced in CI. Put detailed procedures in the relevant required reference;
preserve the rule and its routing trigger. This leaves room below Codex's
32-KiB project-instruction cap (louiselm-ch5b).

## Work tracking and triage

Beads (`br`) is the shared record. Read the graph through `br`/`bvr`, never
by parsing `.beads/*.jsonl`; never initialize Beads implicitly. Durable context
belongs in the repository: issue-local evidence in Beads, standing rules here
or in a required reference, never private agent memory.

- Recommendation requests are read-only: report issue IDs, priority, and one-line
  reasoning, then stop. Claim, implement, or commit only when asked to proceed.
- Run `br robot-docs guide` for installed syntax. Use only `bvr --robot-*`;
  bare `bvr` blocks in a TUI.
- Finish your own claimed work first; another Agent's live claim is protected.
  Claim with `br update <id> --claim --actor "<current session id>"`, never a
  bare status change. Every mutating `br` command requires that attributable
  actor; resolve your own exposed ACP ID with `session.identity()`. Focused
  chat identity, process IDs, historical records and fallback actors are invalid.
  See the workflow reference before mutation; if attribution fails, ask.
- File defects before fixing them, preserving failing evidence. Record unfinished
  or deferred work as issues before handoff. Use dependencies for actual blockers
  and `needs-capability:*` labels for standing capability requirements.
- Search prior lessons with `br search -a`; closed issues are otherwise omitted.
  `br list` does not return comments: use `br show`/`br comments list`.
- Before closing a bug/task/feature, route the lesson to its issue, a required
  instruction, or a `needs-design` question, or explicitly say none is warranted.
  Every close requires exactly one checkable verdict: `consumer:`, `gate:`,
  `live:`, `inert:` (with follow-up), or `none:`. Follow the detailed workflow.

### Bounded query recipes

Filter at the CLI and project fields before output reaches the tool response.
Do not stream complete scheduler/ready payloads or parse tool-truncated JSON.
Limit the combined output of parallel calls too: individual tool budgets do
not prevent the outer response being truncated.

For a next-task recommendation, get a small ranked shortlist:

```sh
br scheduler --limit 5 --json | jq '[.recommendations[] |
  {id: .issue.id, title: .issue.title, priority: .issue.priority,
   labels: .issue.labels, score, rationale}]'
```

The scheduler ranks ready work and excludes claimed issues. For scoped work
discovery use `br ready`, which applies project readiness policy:

```sh
br ready --limit 5 --json | jq '[.[] | {id,title,priority,labels}]'
# Add --label-any needs-capability:<name> for a capability-specific request.
```

Fetch full `br show <id> --json` only for candidates needing detail. Widen the
limit only when the shortlist cannot answer the request; state that a bounded
list is a shortlist, not an exhaustive inventory. Graph-specific questions may
use `bvr --robot-next` or a projected `bvr --robot-triage` result. Inspect the
installed schema/help when a shape is unknown; do not discover it with a raw
backlog dump. These recipes were checked with br 0.5.12.

### Project vocabulary

Canonical meanings live only in Beads `vocabulary/reference` records.
Before semantic work (planning, implementation, review or issue filing), load
this compact index once, then read the full definitions, distinctions and
provenance of applicable terms:

```sh
br list --type vocabulary --status all --json | jq '[.issues[] |
  select(.issue_type == "vocabulary" and .status == "reference") |
  {id,title,description}]'
br show <applicable-term-id> --json |
  jq '[.[] | {id,title,description,notes,design}]'
```

The all-status query intentionally returns an empty result if there is no
vocabulary yet. Pure housekeeping may skip it. Do not scan for missing terms
or treat absence as debt. Do not duplicate definitions into instructions.
Use canonical names for harmless drift. Material conflicts involving vocabulary,
code, issues or user intent require a `needs-design` question and shared
understanding with the user; no source wins automatically. Re-read affected
records immediately before promotion to detect concurrent edits. Never mutate
vocabulary silently or promote before the user's confirmation.

The maintainer's [example workflow](docs/example-workflow.md) is illustrative,
not binding.

## Architecture and lifecycle

- Separate transformation, validation, and state transitions from Neovim UI,
  filesystem, and process I/O.
- `require()` must only define and return APIs. Importing a module must not
  start processes, create buffers, register autocmds/keymaps, or mutate options.
- Explicit `setup`, construction, start, cancel, and dispose own side effects.
- Mutable state has a clear owner/lifecycle, never a hidden module singleton;
  sessions live in the explicit registry.
- Repeated setup/dispose cycles must not leak handlers, buffers, or processes.
- Avoid circular module dependencies. Split modules when responsibilities
  diverge, not in anticipation of future growth.
- A seam that will eventually use process, socket, or filesystem I/O takes an
  asynchronous signature from the start. In-memory doubles use the production
  callback shape too, per [testing](docs/agent-testing.md); synchronous doubles
  must not freeze an unusable contract (louiselm-qbr.3.2.6.5.2).

Core modules never notify, print, open UI, or choose presentation. They return
structured errors and typed events; setup, health, and UI modules display them.

## Errors and validation

- Expected failures return explicit values such as `nil, err` or `false, err`.
  Check both results of every `pcall`; never swallow its error.
- Reserve `error()` for violated internal invariants or clear API misuse.
- Use `assert()` freely in tests, not as production input validation.
- Preserve context internally, sanitize user messages, and make user, config,
  protocol, and process failures specific and actionable.

Configuration uses a closed schema:

- Validate the untouched user table before defaults. Unknown keys/types are
  errors; collect all errors in one pass.
- Never hide invalid input with permissive deep merging or mutate user config.
  Apply defaults only after validation succeeds.
- Deprecations warn with an exact replacement path; do not silently translate
  old keys. Do not keep a removed key in the schema merely to carry that
  message: a declared field is emitted into `lua/louiselm/types.lua` and
  `doc/louiselm.txt`, so the migration note becomes a completion entry
  advertising a key `setup()` always rejects
  (`louiselm-types-advertise-full-content-naz6`). Delete the field and let the
  closed schema reject it as an unknown key, as the project scope above requires.

For ACP/JSON-RPC, validate consumed fields and reject malformed/contradictory
messages, but ignore unknown optional fields from newer peers.

## Security

- Validate all external input before changing state.
- Neither LouiseLM nor maintainer-maintained ACP adapters may read another
  tool's credential store or call a Provider API to obtain Account limits.
  Limits arrive only via an Agent-advertised ACP extension; unsupported is a
  correct result (louiselm-opencode-quota-runtime-ownership-blz13).
- Never log tokens, environments, prompts, tool payloads, or sensitive data by
  default.
- Do not execute generated Lua or use `load`, `loadstring`, the `debug` library,
  LuaJIT FFI, or global/package monkey-patching except Attention's `vim.paste`
  hook; see [Lua policy](docs/agent-lua.md).
- Use `dofile` only for trusted project development files when a module cannot.

## Editing and completion

Preserve unrelated user/agent changes, make the smallest root-cause change,
and revise existing files instead of versioned copies. Never stash user work
or run destructive Git/filesystem commands without explicit instruction.

- Meaningful behavior changes use red-green-refactor. Pure refactors start from
  passing characterization coverage. Documentation/formatting/generated and
  trivial mechanical edits need no artificial red test; explain exceptions.
- Preserve protocol, security, lifecycle, async and public-behavior coverage.
  Removing tests needs a recorded replacement contract; churn is not evidence
  that the old contract is obsolete.
- Run applicable focused checks and the complete affected suite before handoff,
  following [testing](docs/agent-testing.md). Report unavailable gates and
  failures in unrelated in-flight files; do not fix another Agent's work.
- A feature behind optional config needs an unset-case test and verification of
  the maintainer's effective configuration. For live defects, state acceptance
  before implementation and keep the issue open until the maintainer confirms
  the exact reproduction. Automated green gates alone do not close it.
- Update public docs/types. Regenerate types with config schema changes and the
  API appendix with public LuaCATS changes, in the same change. Generators need
  committed, consumed output and a CI `--check` gate.
  Change generated files through their source or generator, never by hand.
- No unnecessary dependency, compatibility shim or speculative abstraction.
  Errors must remain explicit; async work must respect editor scheduling and
  disposal; logs must not expose sensitive payloads.
- Additional development dependencies require a demonstrated gap and explicit
  approval. Never vendor a dependency or utility for convenience.
- Before an authorized commit, run `br sync --flush-only`. Inspect
  `git status --short`, account for unrelated paths, and stage only named paths
  you changed, including relevant Beads records. Never `git add .`/`git add -A`.

If it passes tests but violates an invariant, it is still wrong. If correct but
needlessly complicated, simplify before handoff.
