---
name: louiselm-init-project
description: Initialize a project through a grill-me interview, then create its confirmed brief, agent/workflow/testing/language contracts, Git and Beads foundation, and optional GitLab or GitHub hosting. Use for starting or finishing repository setup; application implementation and full-backlog planning follow separately.
---

# Initialize a project

Leave a documented repository foundation and one actionable first implementation
task. Follow existing authorization; a confirmed brief needs no second interview
or approval ceremony. Local initialization alone does not authorize publishing.
This skill writes project documents and initializes Git/Beads; it does not create
application source, manifests, dependency installations, test harnesses, or CI.

## Establish the starting point

- Read applicable ancestor/repository instructions. Inspect the target directory,
  Git root, status, branch, remotes, and existing `.beads/` before changing them.
  A directory inside another repository is not automatically a new repository.
  Preserve unrelated files, staged changes, history, and remote configuration.
- Read the supplied conversation or brief. Separate confirmed decisions from
  suggestions and open questions. If a source is inaccessible, use available
  evidence and ask about material gaps; complete any explicitly requested
  independent subset and report the missing context. A narrower request controls
  the output; do not turn a small documentation repair into full initialization.
- Use the directory name as the project slug unless the session says otherwise.
  When asked to follow existing projects, inspect their instructions, tracked
  Beads configuration and remotes read-only. The bundled templates distill
  `acp-llm-adapter` and `louiselm`; those checkouts are inspiration, not runtime
  dependencies. Their protocols, fixture paths, privileged tooling and live
  coordination scripts are project-specific.

## Reach a shared project brief

Read and use [louiselm-grill-me](../louiselm-grill-me/SKILL.md). Reuse confirmed
answers and inspect discoverable facts. Ask only about remaining decisions, one
question at a time, with a recommended answer.

Settle what the foundation and first task need:

- Purpose, audience and the first intended user experience.
- Scope, non-goals, constraints, existing implementation and compatibility needs.
- Languages, runtime targets, framework choices when needed, and toolchain
  defaults or overrides. A language choice alone does not select a framework.
- The first working behavior, observable acceptance and how it will be tested.
  Keep this small enough for one actionable implementation task.
- Local repository location and hosting choice: local-only, GitLab or GitHub.
  For hosting, settle the host, owner/namespace, repository, visibility and
  authorization to commit or push. Reuse decisions and permission already given.

Read the selected language templates and propose their defaults together:

| Language | Default toolchain | Template |
| --- | --- | --- |
| Rust | Cargo, rustfmt, Clippy, Rustdoc, Cargo tests | [Rust](assets/agent-rust.md) |
| Lua/Neovim | StyLua, LuaLS/LuaCATS, mini.test | [Lua/Neovim](assets/agent-lua.md) |
| Python | uv, Ruff, ty, pytest | [Python](assets/agent-python.md) |
| TypeScript | npm, strict TypeScript, Biome, Vitest | [TypeScript](assets/agent-typescript.md) |

The user's choices override these defaults. Confirm runtime/tool versions against
the existing project or current official documentation when choosing them; do not
copy a reference project's versions. Load only applicable language templates.
For another stack, use the same policy structure, research its official tooling,
and confirm the proposed policy during the grill; do not silently substitute a
different language or runtime.

Present the grill's compact summary, including its Vocabulary delta, and obtain
its single confirmation. A supplied, already confirmed summary can proceed
directly. Provisional discussion does not authorize files or issue mutations.

For a new project without Beads, the grill still runs: report that vocabulary
cannot yet be loaded or persisted. After confirmation, initialize the explicitly
requested local foundation below **before** the grill's to-beads handoff. This
skill owns that initialization; the grill and router never initialize a tracker
implicitly.

## Initialize the missing local pieces

For an explicitly requested new Git repository, use `git init -b main`. Keep an
existing branch and history unless changing them is part of the request. Resolve
the current attributable session actor before tracker mutations:

- Codex: `codex/<CODEX_SESSION_ID>` when exposed by this interaction; otherwise
  `codex/<CODEX_THREAD_ID>` only after confirming it names this interaction.
- Claude: `claude/<CLAUDE_CODE_SESSION_ID>` when exposed by this interaction.
- Other adapters: their current exposed session identifier and established prefix.

Inspect only relevant environment variables. A username, PID, historical issue
actor, focused editor chat, or generic `assistant` fallback is not attribution.
If the actor cannot be resolved, complete independent authorized Git/document
work and ask for it before tracker mutation. Mark tracker/task links unresolved
and omit commands needing unavailable issue IDs. This partial foundation is not
completed initialization.

Read installed `br robot-docs guide --json` and command help as needed. When
Beads initialization is authorized and no tracker already exists, run:

```sh
br init --prefix <project-slug> --actor "<current-session-actor>" --json
```

Never use `--force` or reinitialize an existing, incomplete, or broken tracker.
Inspect and explain recovery needs separately. Verify `br where` resolves the
intended project rather than an ancestor tracker.

Keep `.beads/.gitignore`, `config.yaml`, `metadata.json`, and the JSONL export
under Git. Use the installed initializer's ignore rules; verify local databases,
WAL/SHM sidecars, locks, history, and recovery files remain ignored. Do not parse
or hand-edit issue JSONL: use `br` for records and `bvr --robot-*` for graph work.

## Record the agreed work

Read and invoke [louiselm-to-beads](../louiselm-to-beads/SKILL.md) with the confirmed
Vocabulary delta and the following explicitly bounded task specification. The
project brief is context, not a request to decompose the entire product:

1. An initialization task covering these documents, tracker setup and any
   authorized hosting. Reuse a matching issue, and claim it with
   `br update <id> --claim --actor "<actor>" --json` before writing documents,
   except for the independent partial setup when attribution is unavailable.
2. One open first implementation task for the agreed working behavior, with
   observable acceptance and the build/test setup needed to verify it. Reuse
   an equivalent existing task. Leave it unclaimed for the next work session.

Keep future ideas in the brief's scope/open decisions until explicitly planned.
Do not create an automatic epic, feature hierarchy or full backlog. Preserve
other agents' claims. Promote only the confirmed vocabulary delta through the
router; `No change` creates no vocabulary records.

## Write the project contracts

Read and adapt the four common templates and the selected language templates:

| Source | Destination | Responsibility |
| --- | --- | --- |
| [Project brief](assets/README.md) | `README.md` | Purpose, audience, experience, scope, stack, milestone and actual status |
| [Root contract](assets/AGENTS.md.template) | `AGENTS.md` | Shared section structure, project invariants and required-reference routing |
| [Workflow](assets/agent-workflow.md) | `docs/agent-workflow.md` | Attribution, task selection, claims, vocabulary and completion |
| [Testing](assets/agent-testing.md) | `docs/agent-testing.md` | Behavioral testing, gate status and acceptance evidence |
| Selected language templates above | `docs/agent-<language>.md` | Language rules, tooling and concrete verification commands |

Adapt directly; no template engine is needed. Replace every `{{...}}` slot with
confirmed context or a clearly recorded open decision. Preserve the root
contract's section order. Include only applicable language links, including
multiple policies for a mixed-language project. Confirmed tooling overrides must
change both the policy and its commands.

The root-contract source uses `.template` so it is not discovered as instructions
governing the skill's own assets. Its generated destination remains `AGENTS.md`.

Extend existing documents and preserve their useful rules. Keep shared policy
in the common documents and language detail in its own reference. Do not copy
project vocabulary into a second canonical glossary.

For an empty repository, state that implementation has not started. Mark quality
commands as **planned**, with their prerequisites owned by the first task; they
are not passing gates yet. For an existing repository, inspect each command and
record its actual status. Do not fabricate entrypoints, installation/usage
recipes, test results, dependencies or CI. A future first task may implement the
documented defaults; initialization itself remains a documentation foundation.

## Host when requested

Read only the selected provider's reference for requested hosting or remote
setup: [GitLab](references/gitlab.md) or [GitHub](references/github.md).
Follow its checks and the user's existing authorization. If the destination is
unsettled, finish independent local work and ask only for the missing choice;
do not infer a provider or owner from available credentials or switch hosts
after an access failure. Local-only setup needs neither provider nor login.

## Verify and hand off

Flush Beads explicitly and review `git status --short`, the diff, and ignore
behavior before staging only owned paths. Include relevant Beads export changes;
do not sweep concurrent issue edits into an authorized commit. Use a concise
Conventional Commit and `Refs <issue-id>` / current session references when
applicable. Do not manufacture executable tests for documentation-only setup.

Check document links, unresolved template slots, selected-language coverage and
agreement between README status, planned gates and the first task. Verify
tracker location/status, ignore rules, the selected branch and first-task
readiness. Run actual existing gates when applicable.

Close only the initialization task, with concrete evidence that its requested
scope is complete. Missing required access or acceptance keeps it open. Export
with `br sync --flush-only --actor "<actor>" --json`, then verify
`br sync --status --json`. Sync does not commit or push.

After a requested push, verify the remote branch matches the local commit and
the selected host reports the intended owner/namespace, visibility and default
branch. Report the repository URL or local path, document set, checks, first
implementation issue ID and remaining gaps. Distinguish validated foundation
checks from planned application checks.
