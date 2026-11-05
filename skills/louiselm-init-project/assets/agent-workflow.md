# Agent workflow

Required by [AGENTS.md](../AGENTS.md) for tracker mutations, task selection,
claims and session attribution. Read once before applicable work.

## Shared record

Beads (`br`) holds work, decisions, evidence and lessons. Its JSONL export is
versioned; local database, lock and recovery files stay ignored. Use `br` for
records and `bvr --robot-*` for graph queries. Never parse or hand-edit issue
JSONL. A missing or broken tracker needs diagnosis, not implicit initialization.

Issue-local evidence belongs in Beads; standing conventions belong in the
applicable repository instructions. Keep durable context accessible to the
next agent, not solely in a private memory or chat transcript.

## Session attribution

Resolve an actor at the start of each session, not from the identity of the
agent that initialized this repository. It must identify this interaction's
transcript:

- Codex: `codex/<CODEX_SESSION_ID>` exposed by the current interaction, or
  `codex/<CODEX_THREAD_ID>` when confirmed to identify this interaction.
- Claude Code: `claude/<CLAUDE_CODE_SESSION_ID>` from this interaction.
- Other adapters: their exposed current session identifier and established
  actor prefix. If a host provides an identity resolver, resolve your own ID,
  not whichever conversation happens to be focused.

Inspect only relevant environment variables. A username, PID, historical actor,
session listing or generic `assistant` fallback does not identify the caller.
Ask for missing attribution before a tracker mutation; continue independent
read-only work when useful.

The examples below use `$PROJECT_ACTOR` for this resolved value. Pass
`--actor "$PROJECT_ACTOR"` on every mutation, including creates, claims,
comments, dependencies, labels, closes and explicit exports.

## Task discovery and claims

A specific user request takes precedence over automatic selection. Finish your
own in-progress work before claiming another item. Inspect the current issue
and its acceptance before implementation.

Use installed command help or `br robot-docs guide --json`. Start with bounded
queries; increase limits only when the shortlist cannot answer the request:

```sh
br list --assignee "$PROJECT_ACTOR" --status in_progress --limit 5 --json
br ready --limit 5 --json
br show <candidate-id> --json
```

`br scheduler --limit 5 --json` may rank candidates when available. Use
`bvr --robot-next` or a bounded/projected `bvr --robot-triage` result for
graph-aware decisions; never run bare `bvr`. Inspect help/schema before
projecting unfamiliar JSON fields. Preserve command failures with
`set -o pipefail` when piping output. Keep combined parallel output bounded
and never parse tool-truncated JSON.

Recommendation requests are read-only: report IDs, priorities and reasons.
When asked to implement, reuse a matching issue or create one and then claim:

```sh
br update <id> --claim --actor "$PROJECT_ACTOR" --json
```

Use the atomic claim, not a bare status change. Another agent's live claim
belongs to that agent. Age alone does not authorize takeover; report uncertainty
or have the maintainer resolve ownership. Do not copy another project's
liveness scripts or treat an empty coordination snapshot as evidence.

Standing capability requirements may use `needs-capability:<name>` labels;
actual blockers use dependencies. Task selection does not assign models or
agents automatically. Avoid circular or opposing aggregate dependencies.

## Project vocabulary

Before semantic planning, implementation or review, load the vocabulary once:

```sh
br list --type vocabulary --status all --format toon
```

Use only `vocabulary/reference` records and read applicable definitions.
An empty result is valid. Do not scan for missing terms or duplicate the
canonical glossary into documents. Harmless naming drift may be normalized;
material contradictions between intent, records or code need a focused
decision. Keep proposed changes provisional until the user confirms them.
Re-read affected records before promotion to avoid overwriting concurrent edits.

## Filing, verification and completion

File defects with observed and expected behavior before fixing them. Record
unfinished or deferred work with its next action; an unresolved design decision
may need a `question` labeled `needs-design`. Use dependencies for real
blockers, not ordering preferences.

Search prior lessons with `br search -a "<terms>" --limit 5 --json`; closed
issues are excluded without `-a`. Read full evidence with `br show` or
`br comments list`; a list result is not the comment history.

Close only when acceptance is met. State the behavior/check that proves it and
the actual result; planned commands and syntax checks do not prove runtime
behavior. If acceptance requires a live reproduction, leave the issue open
until that reproduction is verified. Route useful lessons to the issue, a
standing instruction or a design question; explicitly say when none is needed.

```sh
br close <id> --reason "<completed behavior and verification evidence>" --actor "$PROJECT_ACTOR" --json
br sync --flush-only --actor "$PROJECT_ACTOR" --json
br sync --status --json
```

Export before handoff and any authorized commit. Sync does not run Git.
Inspect `git status --short` and the diff, preserve unrelated work, and stage
only named task-owned paths and relevant Beads changes. If the export includes
other sessions' edits, use an existing supported selective staging workflow or
resolve the scope before committing it whole. Never use blanket staging.
Commit and push only when authorized by the session.
