---
name: louiselm-sessions
description: Launch a LouiseLM Session for any task or topic in the current Neovim. Use when asked to start a new Session, delegate work, or monitor explicitly selected Session operations.
---

# LouiseLM Sessions

Derive the Session's brief from the user's task or topic: implementation,
investigation, planning, writing, review, QA, or another purpose. The task
determines the deliverable, allowed changes, relevant skills and stopping point.
For interactive work, have the child open the discussion and wait for the user;
for autonomous work, have it complete the scoped task and report the result.

Use the configured owner's controls through `$NVIM`. Find the installed helper
once; if the socket, helper or control API is unavailable, report that boundary.
Agent-created Sessions must stay in the background. Use the structured helper;
never launch through `:LouiselmSessionNew`, open or drive pickers, switch focus,
or simulate keyboard input. The helper owns background attachment and ordered
configuration. A missing control capability is not a reason to reconstruct the
owner or improvise an interactive launch path.

```sh
control=$(nvim --headless --server "$NVIM" --remote-expr 'luaeval("vim.api.nvim_get_runtime_file(\"scripts/session-control\", false)[1] or \"\"")')
```

Resolve your own exposed ACP ID with `require('louiselm.session').identity(id)`;
focus does not establish identity. Extract the objective, cwd, configured Agent,
requested options, allowed effects, deliverable and stopping condition. Default
one-off delegation to one child and one initial turn. Reuse choices already
given in the conversation; ask only for missing details needed to launch.

Create `request.json` with the file-edit tool, adapting this data:

```json
{
  "id": "<own-acp-id>:task-1",
  "agent": "<configured-agent-name>",
  "cwd": "/path/to/project",
  "name": "<short task or topic name>",
  "options": [],
  "prompt": "Read AGENTS.md if present. <Task or topic and relevant context>. <Scope and authorized changes>. <Expected deliverable and when to stop or wait for the user>.",
  "timeout_ms": 300000
}
```

Adapt the brief to the request, for example:

- Implementation: implement ISSUE, run its required checks and report changes
  and results.
- Planning: discuss TOPIC, propose the first decisions to explore and wait for
  the user's input.
- Human QA: use `louiselm-qa-review` for ISSUE at COMMIT, produce the first test
  round and wait for the maintainer.

Choose a fresh operation `id` before dispatch and retain it. `options` is an
ordered array of `{ "id": "<advertised-option>", "value": "<requested-value>" }`;
values may also be booleans. Apply Model before dependent effort settings.
If labels need resolving, query existing advertisements, narrowing at each step:

```sh
"$control" discover
"$control" discover '<configured-agent>'
"$control" discover '<configured-agent>' '<local-session-id>' '<option-id>'
```

Omit the last selector to see that Session's options. Discovery never starts a
child or changes an existing Session. Check its current Model before interpreting
dependent effort choices; fresh child advertisements are validated during launch.
Unavailable or ambiguous choices require exact user-supplied values, not guesses,
probe children, changing another Session's options, or UI/source scraping.
Specify permission options only when selected by the user; preserve configured
policy otherwise. The control verifies requested options before submission.
Optional `context` is an explicit array of `{ "name": "...", "text": "..." }`;
never borrow the human's staged draft. Set `parent_turn_id` only to an actual
parent turn ID. Keep briefs compact with issue/commit references.
`timeout_ms` bounds the entire operation through first-turn completion (default
300000, maximum 3600000); expiry disposes the owned child. Explicit unlimited work
uses `"timeout_ms": false`, not omission. On an older installation that rejects
this capability, report the boundary; never silently choose a finite deadline
or bypass the helper through UI or direct Session calls.

For several tasks, create one batch manifest and invoke `batch` once. Shared
fields apply to every child; `tasks` contains 1–16 distinct IDs and briefs:

```json
{
  "agent": "<configured-agent>",
  "cwd": "/path/to/project",
  "branch": "main",
  "options": [],
  "timeout_ms": false,
  "tasks": [
    {"id": "<own-acp-id>:task-1", "issue": "ISSUE-1", "name": "Task one", "prompt": "Read AGENTS.md. Complete ISSUE-1; claim with your own Session identity. Work on main, preserve others' edits. Run required checks and report."}
  ]
}
```

Adapt scope and settings to the user's request. `branch` asserts the existing
branch before starting; it never switches it. `issue` is an output label, not a
claim. The helper validates all tasks first, launches/configures them through
the owner, and prints one compact JSON line per operation after dispatch or
failure. Partial results stay attributable to the original IDs.

```sh
"$control" batch batch.json --timeout-ms 60000
```

This client deadline only bounds waiting for dispatch; children keep their own
deadline setting. Reconcile every unknown result by its original ID. A batch
retry must use the identical manifest; never regenerate IDs to retry.
For "next best tasks", follow the project's tracker contract: select ready,
unclaimed leaves using bounded metadata (IDs, titles, priority, labels and
dependencies), then inspect only candidates whose scope remains unclear. Leave
task-specific source reads and implementation checks to the children. Keep
shared instructions referenced and briefs compact; do not copy the project
contract into every prompt. Exclude blocked/claimed parents and human-only QA
or capability-ineligible work; retain reasons when exclusions matter. Do not
claim children's tasks yourself. Return IDs, phases and confirmed options.

```sh
"$control" start request.json
"$control" watch '<operation-id>' --timeout-ms 60000
"$control" inspect '<operation-id>' --result
```

Watch prints changed JSON only and waits at most 60 seconds per call; its local
timeout does not cancel the child. Set a finite monitoring deadline. `--result`
adds `result.text` (last 8192 bytes) and `result.truncated`; read it only when
needed. For a start-only request, verify dispatch and requested settings, then
return; monitor through completion only when requested. For authorized
cancellation of owned work:

```sh
"$control" cancel '<operation-id>'
```

Reconcile lost acknowledgments with `inspect` using the original operation ID.
Never submit a new ID to retry an uncertain start. Operations live in the
current editor; a missing record after restart is uncertainty, not permission
to recreate the child.

Separate admission, dispatch, permission waits, turn completion and task
acceptance. `ready`, `completed`, silence or missing state does not prove the
deliverable was accepted. Report operation/Session/turn IDs, confirmed options,
outcome and the next required action. Retain children for requested human
follow-up; clean up only owned work. Ordinary delegation supplies neither
containment nor a Run budget. Use existing Run supervision for bounded workflows.
