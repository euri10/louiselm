# Session delegation: recipe and AX review

This checkout provides `require("louiselm").session_control(request)` and
`scripts/session-control` for scoped delegation through the editor's configured
Session owner. The recipe comes from the maintainer's 2026-10-13 Botanotype QA
Session. Implementation is tracked in **louiselm-6twgs**, with the prompt-history
defect in **louiselm-jzw3h**. Ordinary delegation does not establish containment,
a capability envelope, or a Run.

The `louiselm-sessions` skill is installed from the maintainer's dotfiles catalog.
Updating a checkout or installing a skill does not replace loaded Lua modules.
Use the normal plugin update and editor restart before invoking the new API;
a missing API is reported explicitly. Implementation and offline verification
do not modify the maintainer's live editor.

## What happened

Parent `codex/01a133f2-4eb1-7fe2-91c4-69566beb7056` created child
`codex/01a1342b-4d8c-7510-a0d5-b46741cdcbc0` after the maintainer requested
a separate QA Session using Astra / max / Full access in the same Neovim.

| Observed step | Evidence / significance |
| --- | --- |
| Discover running editor and APIs | `$NVIM`, installed code reads, current Session inspection |
| Create visible child | `LouiselmSessionNew`, inspect picker window, select `codex`, find new ACP ID |
| Configure child | Rename; set `model=gpt-6-astra`, `reasoning_effort=max`, `mode=agent-full-access` sequentially |
| Check before dispatch | Require `ready`, `current_turn=0`, and all three confirmed option values |
| Send scoped assignment | Direct `Session:prompt()` with a 2,332-byte task brief |
| Verify first turn | Inspect child status, turn identity/options and buffer |

The setup turn contains **26 tool calls**, including **12 Neovim RPC calls**
whose command strings total **9,903 UTF-8 bytes**. These are observed command
counts/bytes, not token usage or a prediction of savings. Most of the avoidable
work was discovering the control path, driving the picker, and repeating lookup
and option checks. The task brief itself carried useful scope and acceptance.

Primary local evidence (private logs remain in place):

- Parent: `~/.local/state/acp-llm-adapter/proxy/sessions/01a133f2-4eb1-7fe2-91c4-69566beb7056/log.jsonl`.
  Creation request is the user turn at `07:25:01.763Z`; dispatch tool ID is
  `exec-e5eba311-87d7-4ebe-bcb1-d3de9c08ca6a`.
- Child: same root, `sessions/01a1342b-4d8c-7510-a0d5-b46741cdcbc0/log.jsonl`:
  option requests at lines 3/5/7; `session/prompt` at line 9, `07:33:44.572Z`.
- Construction: same root, `connections/c9b833a4-980f-4812-a213-4eea48a2224b.jsonl:4`.
  It records `session/new` with `/home/lotso/code/botanotype` as cwd.
- Screenshots: `~/Pictures/scrot/20261013_0934_1920x1200_1791876881.png`
  (child) and `20261013_0934_1920x1200_1791876892.png` (parent, line 828).

The complete original prompt reached the Agent. Its absence from the child
chat is **louiselm-jzw3h**: direct Session submission bypasses Chat's pending
submission, local transcript record and staged-context consumption. The
`> [context: AGENTS.md]` line is an untouched composer. It is not the submitted
assignment. The wire prompt has one text block, not that queued attachment;
native Agent instruction loading is a separate mechanism.

The diagnosis used installed revision `025a0c5e`, before this fix.
See `lua/louiselm/ui/chat/init.lua` (`submit_prompt`, `accept_prompt`,
`settle_prompt`, `Chat:new_session`), `session/lifecycle.lua` (live user-echo
suppression), and `session/transcript.lua`. The live Chat's export/Handoff also
depends on that missing local transcript entry. Resumed history was not tested.

## Concrete recipe

Resolve the caller with `require("louiselm.session").identity(own_acp_id)`.
Choose one objective, workspace, configured Agent, permitted effects, deliverable
and stopping condition. A one-off QA delegation needs one child and one initial
turn, retained for human follow-up. Keep the operation ID before dispatch.

Find the installed helper once, through the editor named by `$NVIM`:

```sh
control=$(nvim --headless --server "$NVIM" --remote-expr 'luaeval("vim.api.nvim_get_runtime_file(\"scripts/session-control\", false)[1] or \"\"")')
```

Create `request.json` as data with the file-edit tool:

```json
{
  "id": "<own-acp-id>:qa-1",
  "agent": "codex",
  "cwd": "/home/lotso/code/botanotype",
  "name": "Botanotype QA",
  "options": [],
  "prompt": "Read AGENTS.md and required references. Use louiselm-qa-review for ISSUE at COMMIT. Prepare the first human QA round, then wait for the maintainer. No fixes, commits, paid calls, hardware operation or further children.",
  "timeout_ms": 300000
}
```

Use `options` only for requested, advertised choices, as an ordered array of
`{"id":"…","value":"…"}`; values may also be booleans. Model changes precede
dependent effort settings. The observed QA choice was `model=gpt-6-astra`,
`reasoning_effort=max`, `mode=agent-full-access`; those are evidence for that
Agent, not universal option IDs. The controller confirms each replacement list
and the final tuple before dispatch. It preserves configured permission policy;
an Agent's upstream access mode and LouiseLM's approval policy remain separate.

Optional `context` contains explicit `{ "name": "…", "text": "…" }` snapshots.
It never borrows the human's draft or staged `AGENTS.md` attachment. Native Agent
instruction loading remains Agent-owned; the configured Skill policy and an
injected catalog, when selected, are preserved. Add `parent_turn_id` only when
the actual parent admission/turn ID is known; it supplies correlation.

```sh
"$control" start request.json
"$control" watch '<own-acp-id>:qa-1' --timeout-ms 60000
"$control" inspect '<own-acp-id>:qa-1' --result
"$control" cancel '<own-acp-id>:qa-1'
```

`start -` reads bounded JSON from stdin. `cancel` is for authorized cancellation
of the selected operation; it is not a required final step. The Lua equivalent
is `require("louiselm").session_control(request)` with `action="start"`,
`"inspect"` or `"cancel"`. Inspect and cancel require only `action` and `id`;
inspect additionally accepts `result=true`. The API returns `snapshot, nil` or
`nil, error` and starts asynchronous work without blocking the editor. Request
and snapshot fields are documented in the generated public API appendix.

The child gets its own Chat buffer and appears in the Session switcher without
taking focus. The owner reuses configured Agents, MCP selection, permission
handling and Session lifecycle. Initialization, ordered configuration and one
initial prompt are one operation. Completed children stay available for human
follow-up; later human turns do not reactivate that operation.

Snapshots contain operation/Session/turn IDs, `revision`, `phase`, latest
Session `status`, admission result, confirmed options and bounded errors.
`options` contains confirmed values, not an advertised-choice catalog. The
control surface has no option-discovery operation. If requested labels cannot be mapped to
known advertised IDs/values, ask for exact choices rather than guessing or
launching probe children. Compact discovery is follow-up **louiselm-cozqr**.
Watch prints only changed revisions and terminal state. Use `--result` when needed:
it adds an assistant-text tail and a truncation flag, not tool payloads or the
complete transcript. A returned turn ID means admission began; `ready` means
idle; `completed` means the initial turn ended. None certifies task acceptance.

### Limits and recovery

| Boundary | Limit or behavior |
| --- | --- |
| Operation records | 128 per configured owner registration; memory only, retained after completion/failure |
| Operation ID | 128 bytes; exact repeat returns the existing record; different request with the same ID refuses |
| Task/context | Prompt at most 64 KiB; at most 16 named text snapshots; combined prompt/context at most 256 KiB |
| Options | At most 32 distinct IDs, processed in caller order |
| Results/errors | Assistant tail at most 8192 bytes; error at most 1024 bytes |
| Child deadline | `timeout_ms` covers creation through initial-turn completion; default 300000, maximum 3600000; expiry disposes the child |
| Local watch | Default/maximum 60000 ms per call; expiration does not cancel the child |
| CLI transport | JSON file/stdin at most 1 MiB; ordinary RPC calls have a 5-second client deadline |

Deadline expiry retains the reason in the Session snapshot and chat, and the
operation reports the child's final `disposed` status. In a stopped ordinary
Session with an ACP identity, click **Resume this Session** or run
`:LouiselmResume` to load that conversation directly. The old view, draft and
staged context survive failed loads; successful loading replaces the view and
keeps the latest unsent input. Recovery never submits the interrupted prompt or
a queued follow-up automatically. Managed Sessions use their Run recovery.
`:LouiselmResume!` always opens the history picker across all workspaces.

The CLI sends a fixed Lua call with request data as RPC arguments. A separate
disposable client enforces deadlines even when the hosting editor stops
responding. A timeout reports the original ID and unknown outcome: inspect
that ID before retrying. Never allocate a new ID to retry an uncertain start.
After editor restart or command re-registration, operation records are gone;
an unknown ID is not permission to recreate a possibly existing child.

Cancellation while active requests normal Session cancellation; cancellation
before dispatch prevents later configuration callbacks from sending the prompt.
Do not equate a cancellation request with acknowledgement. Watch distinguishes
`cancelling`, `cancelled`, `failed` and `timed_out`; permission waits remain
visible in the Session status. Choose a finite monitoring budget independently
of the child's deadline. No live snapshot or local disposal return proves
containment or remote Provider cancellation.

## AX rationale and skill ownership

The configured-owner facade replaces source archaeology, picker keystrokes,
window-focus targeting and repeated Session lookups. Its implementation stays
private; callers need only data, operation IDs and the public reference.
Existing typed events feed compact records. Operation ownership releases its
observers and timers at completion, cancellation, disposal or deadline.
Prompt payloads and result tails are not added to default audit logs.

The **louiselm-jzw3h** fix observes dispatched prompts at the shared Session
boundary. Chat can record/render direct submission once, preserve unrelated
drafts and deduplicate live ACP echoes. Rejected or pre-dispatch-cancelled
prompts create no submitted history; cancellation after dispatch retains it.
The maintainer's original screenshot reproduction still requires live acceptance.

For multiple workers or durable supervision, compose the existing
`workflow.Run` ownership/cancel/Park lifecycle and generated-work ledger.
`parent_turn_id` is correlation, not a budget or authorization. Starting a new
task is delegation; canonical **Handoff** remains a takeover with the operator
instruction and reviewed compacted transcript. The skill translates intent
into existing controls; it owns no second scheduler, policy or durable store.

`louiselm-sessions/SKILL.md` is maintained in the dotfiles Agent Skills catalog
and generated/installed through that catalog's normal workflow. It points to
the installed helper and keeps mechanics in plugin code. This guide documents
the implementation and evidence; it does not duplicate the installed skill.

The compact brief from the observed task remains a useful example:

> In /home/lotso/code/botanotype, use louiselm-qa-review to prepare the first
> human QA round for commit 931418e / botanotype-resin-workbench-252.8.6.
> Read AGENTS.md and required references. Check complete-set selection, immutable
> export retrieval after restart, and historical/stale handling. Prior software
> gates reportedly passed; physical printer acceptance remains unverified.
> Preserve other Sessions' A4 work. Deliver the QA plan path and first test
> steps, then wait for the maintainer. No fixes, commit/push, paid AI calls,
> printer operation or further children. Record confirmed bugs under your own
> Session identity.

## Verification

| Slice | Acceptance |
| --- | --- |
| Prompt visibility (louiselm-jzw3h) | Direct submission appears once before its answer and in Chat export; unrelated draft survives. Rejection or cancellation before dispatch creates no submitted history; cancellation after dispatch retains it. ACP echoes never duplicate it. Maintainer confirms the screenshot reproduction. |
| Configured-owner controls | Two concurrent children receive their own cwd/options/brief despite focus changes. Missing options, initialization/configuration failures, permission waits and uncertain writes yield bounded, attributable results. Retry cannot duplicate an admitted operation. |
| Skill packaging | The installed catalog skill performs the same QA delegation without source archaeology or picker manipulation; report measured calls/bytes separately from tokens. |

Executable slices follow `docs/agent-testing.md`: red/green focused Session and
Chat regressions with the mock ACP Agent, complete Lua suite, StyLua, LuaLS,
generated API/type/doc checks, and applicable installed gates. The skill needs
one realistic invocation and maintainer confirmation of visible prompt/context.
Disposable mock-Agent checks are not evidence of live Provider behavior or the
maintainer's installed plugin revision. Documentation and generated output need
no artificial failing behavior test.

The installed skill was exercised by an independent consumer in a disposable
editor with an offline ACP peer. Five command invocations (two helper/identity
queries, start, watch, result inspection) used 866 UTF-8 command bytes and an
809-byte request. The supplied advertised Model/effort/mode were confirmed;
one prompt appeared before its reply, and the parent buffer/focus survived.
This differs from the historical task and is not a token-savings measurement
or evidence that a real QA deliverable was accepted.

AX checklist reviewed: clear Session/Chat/Run ownership and dependency direction;
explicit IDs and authority boundaries; one configured control path; state before
mutation and outcomes afterward; bounded errors/retries/cleanup; discoverable
catalog entry and API reference; event-based, bounded output; existing lifecycle
and tests reused; delivery ordered from visible correctness to usable controls
to skill packaging. The documented request schema and CLI are the supported
control surface; UI ownership and transport mechanics stay implementation details.
