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
initial prompt are one operation. Failed or cancelled pre-dispatch children are
disposed, and their untouched background views are removed. Chat keeps the
current view and views with a draft, staged context, a selected skill or queued
work. Completed children and children whose dispatch is uncertain stay available
for human follow-up. Later human turns do not reactivate that operation. If a
different turn takes over a child, a delayed failure, cancellation or operation deadline
leaves that turn alone and reports the child's current status.

Snapshots contain operation/Session/turn IDs, `revision`, `phase`, latest
Session `status`, admission result, confirmed options and bounded errors.
`options` contains confirmed values, not an advertised-choice catalog.
Watch prints only changed revisions and terminal state. Use `--result` when needed:
it adds an assistant-text tail and a truncation flag, not tool payloads or the
complete transcript. A returned turn ID means admission began; `ready` means
idle; `completed` means the initial turn ended. None certifies task acceptance.
For a failed or timed-out operation, `status` reports the child's actual state
after attempted cleanup. A cleanup error is reported alongside the failure;
`disposed` alone does not prove external resources, such as a VM started by the
Agent, have stopped.

### Read-only option discovery

```sh
"$control" discover
"$control" discover codex
"$control" discover codex '<local-session-id>'
"$control" discover codex '<local-session-id>' model
```

These return configured Agent names, then candidate Session IDs/current values,
then an explicitly selected Session's advertised option IDs, labels, values and
current selections. The last selector narrows to one option. The Lua request is
`{action="discover", agent=..., session_id=..., option_id=...}`; selectors are
optional from right to left. Responses omit descriptions and all configuration
commands/environments; they cap Sessions at 32 (with `truncated`) and the encoded
response at 64 KiB. Oversized advertisements require narrower selectors.

Discovery never initializes an owner or child, changes a Model, sends a prompt,
or opens UI. `availability` is `unavailable`, `select_session` or `available`.
No existing advertisement means explicit unavailability, not permission to spawn
a probe. When interpreting dependent effort choices, inspect the source
Session's current Model. Existing advertisements are advisory: the launch
controller validates every choice against the child's fresh, ordered replacement
lists, refusing dispatch if a value disappeared or was not confirmed. Unknown
labels require clarification, not invented option IDs. A failed operation does
not include an advertised-choice catalog, and discovery remains unavailable when
no existing Session advertises one.

### One-command background batches

Create a manifest with shared settings and 1–16 selected task briefs:

```json
{
  "agent": "codex",
  "cwd": "/path/to/project",
  "branch": "main",
  "options": [],
  "timeout_ms": false,
  "tasks": [
    {"id": "<own-acp-id>:task-1", "issue": "ISSUE-1", "name": "First task", "prompt": "Read AGENTS.md. Claim ISSUE-1 with your own Session identity, implement it on main, preserve unrelated edits, run required checks and report."},
    {"id": "<own-acp-id>:task-2", "issue": "ISSUE-2", "name": "Second task", "prompt": "Read AGENTS.md. Claim ISSUE-2 with your own Session identity, implement it on main, preserve unrelated edits, run required checks and report."}
  ]
}
```

```sh
"$control" batch batch.json --timeout-ms 60000
```

Supply the Agent's actual advertised options; the example's empty list preserves
configured defaults. Shared fields are `agent`, `cwd`, `options`, `timeout_ms`,
`context` and `parent_turn_id`. Each task accepts `id`, `name`, `prompt` and an
optional `issue` output label. All inputs validate before any RPC. Optional
`branch` checks the current Git branch before launch; it never switches branches
and does not lock Git against concurrent changes. No task is claimed by the
coordinator. Task choice stays in the project's bounded `br scheduler`/`br ready`
workflow; batch execution neither ranks work nor guesses capability eligibility.

The command starts/configures children via the existing owner and emits one JSON
line per operation after dispatch or failure. It exits nonzero on any failure
or uncertainty, retaining successful children. Its local wait is at most 60
seconds and does not cancel children, even with `timeout_ms: false` in the
manifest. Permission waits remain visible. After an unknown result, inspect the
original ID; never generate a replacement ID. Exact manifest retries reuse
existing operations, including failed ones. Completed children remain available.

The offline five-child CLI gate measured **one helper invocation, 1,610 output
bytes and 320 ms** on this host. It also checks exact retries, selected options,
cwd, `main`, one visible prompt per child, and unchanged draft/focus/windows.
The historical live launch used 34 model responses, 49 shell invocations,
280,648 retained tool-output bytes and 575,309 ms. These are different workloads
(offline mock versus live Agents); this proves the deterministic launch path,
not a real-Provider speedup or token/billing savings. Worker implementation
consumption is outside the launch measurement.

### Limits and recovery

| Boundary | Limit or behavior |
| --- | --- |
| Operation records | 128 per configured owner registration; memory only, retained after completion/failure |
| Operation ID | 128 bytes; exact repeat returns the existing record; different request with the same ID refuses |
| Task/context | Prompt at most 64 KiB; at most 16 named text snapshots; combined prompt/context at most 256 KiB |
| Options | At most 32 distinct IDs, processed in caller order |
| Results/errors | Assistant tail at most 8192 bytes; error at most 1024 bytes |
| Child deadline | `timeout_ms` covers creation through initial-turn completion; default 300000, maximum 3600000; `false` explicitly means no deadline; expiry disposes a still-owned child |
| Local watch | Default/maximum 60000 ms per call; expiration does not cancel the child |
| CLI transport | JSON file/stdin at most 1 MiB; ordinary RPC calls have a 5-second client deadline; batch dispatch waiting is at most 60 seconds |

For an explicitly unlimited task, set `"timeout_ms": false`. No operation timer
is allocated. Cancellation and owner disposal still stop owned work and release
observers. This does not extend the bounded client/watch waits or provide Run
supervision, containment or a durable operation record.

Deadline expiry retains the reason in the Session snapshot and chat when it
disposes the child, and the operation reports the child's actual final status.
In a stopped ordinary
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

## Following up and closing delegated Sessions

Design review for **louiselm-6twgs.3**, 2026-11-26.

### What works today

The installed package (`20096991`) and checkout (`a1d5465c`) have identical
helper and controller files. `start`/`batch` launch children, `discover`
reads advertisements, and `inspect`/`watch` report the original operation.
In `lua/louiselm/ui/session_control.lua:530`, normal success calls `finish`
without cleanup. It releases the observer and clears `op.session` (407–417).
Terminal `cancel` reaches disposal only if unsuccessful cleanup retained that
reference (668). Normally it returns the frozen result. Active cancel targets
the owned turn.

Humans can submit in Chat and use `:LouiselmSessionClose` with explicit local
IDs and Close/Keep confirmation. `Chat:submit(text)` writes the current
composer. The orchestrator needs separate operation, Session and issue lookups,
plus slot status for a VM. The helper has no addressed follow-up or verified close.

### Proposed controls

**1. Can the orchestrator send the next message?** Add `followup FILE` to
`scripts/session-control`. Extend `inspect <launch-id>` with live parent/child
ACP identities, latest turn, revision, blockers and permitted action alongside
the original result. Bind control to the verified caller and operator's scope
at launch. Neither the key nor `parent_turn_id` grants authority. When 53
became 76 the same ACP actor could continue monitoring (`louiselm-qbr.12:5696`).
A different actor needs an explicit transfer naming child, issue, turn and scope.

Both new commands take an action `id`, original `target` launch ID and
`if_revision` from live inspect. At the point of action the configured owner
rechecks registration, child ACP, turn and Chat input/queue state. Follow-up
sends explicit text/context through the idle Session's admission and Chat's
`prompt_dispatched` event. Return the new turn ID and delivery outcome.
A human takeover, draft, staged skill/context, queued work, active turn or
pending decision yields a conflict. In 55 the original `qbr12-recovery-1`
was terminal (`failed/ready`), yet the person who continued it still had a
live worker on `louiselm-qbr.12.4.11`.

Identical retries return the original receipt. Changed input conflicts. Inspect
the same action ID after a lost reply. A lost registration leaves delivery
unknown. Reconcile with the live owner and issue before explicit rebinding.
A stopped, loadable conversation already has the operator-directed Resume path.

**2. Can it close a finished child?** Add `close FILE` with the same guards
and a reference to accepted work or a handback. The skill checks the delivered
scope or recorded remainder, claim disposition and resource receipts. The
owner checks for a later turn, visible human work, draft, queue and pending
decisions. Missing proof or takeover returns “keep” with a reason. Await the
`Session:dispose` callback and saved-queue fence, recheck the view, and report
Session disposal and view removal or retention separately from external
cleanup. A missing view without an owner receipt, or a failed callback, leaves closure unconfirmed.

A grill can launch with “return on final confirmation.” Its usual final
“Confirm and return” records decision revision, Session and turn and authorizes
closing that view once idle. A new draft, later human turn or “keep discussing”
wins. An unplanned takeover lasts until the person hands the child back.

74's operation still reads `completed/ready` after manual close. Its exact
`8df311cf` gates, two returned claims and cleaned VM established handback while
integration stayed open (`louiselm-qbr.12.4.15.5:5913,5917`,
`louiselm-qbr.12.4.15:5920`). When issue, inputs, environment, issuer and
result match, consume confirmed evidence directly. Examples include the
confined controls in `louiselm-6twgs.1`, human-confirmed prompt/draft/export
in `louiselm-6twgs.2` and `louiselm-jzw3h`, two-guest pairing receipts in
`louiselm-qbr.12:5661` and the host-scope decision in `louiselm-qbr.12.4.13`.
Installed writer integration, physical acceptance (`louiselm-qbr.12.22`) and
historical-cause judgment (`louiselm-qbr.12.48`) have separate gates.

**3. What if the worker cannot take another turn?** 46 needed a maintainer
relay after a policy interruption and held a disposable VM until handback
(`louiselm-qbr.12.48:5360`). This supplied no quota diagnosis. A failed turn
can leave a ready Session and record measured usage once. Surface the Session's
typed failure (ID, revision, severity, title) and admission in the receipt.
Today the helper reduces the diagnostic to text. Show an unclassified title as such.

| Evidence | Next action |
| --- | --- |
| Reported temporary Model/Provider capacity | Honour a retry time or bounded backoff. With authority, try one same-Model continuation within the deadline after checking what ran. Hand back if still unavailable. |
| Fresh exhausted bucket from Agent-advertised ACP `accountLimits` | Show the window and reset. Return held work before a long wait. Recheck at reset or use an approved alternative. Extra spend, account or Provider changes need explicit authority. |
| Refusal, uncertain delivery or missing/stale limits | Respect the stop. Reconcile action, turn and tool effects. Never replay potentially effectful work or infer quota from tokens or context occupancy. |
| Local limit | At 128 operation IDs, inspect existing work and stop allocating IDs in that registration. At the VM cap/disk floor, wait or release an owned slot. Closing a Chat frees neither reservation. |

Wake the parent ACP's bounded watch with a typed change. On reconnect inspect
outstanding conditions by child/turn/revision. After registration loss, use
recorded turns, Beads and slot receipts, retaining unknowns. Only when a person
must act, show one updated status with cause, retry/reset time, held resources
and the decision needed. Reuse the Session-failure Attention item where applicable.

The child records the commit, evidence and remaining work, then returns its own claim with
`br update --if-unchanged`. The verified VM holder runs
`scripts/launcher-vm --slot cleanup` and checks the allocation is absent and
unit inactive. To release without a Model turn, let the holder enroll a
release-only action at allocation. Under the slot locks recheck owner, exact
allocation, quiescent gate and permission to discard. Keep base and logs.
Existing `--slot cleanup --gone` checks the whole pool for provably gone owners.
Protect live/unknown holders and retained/persistent guests.

A future exact-claim return without a Model turn needs tracker support for
one-use authority from the holder, bound to issue, assignee and version, audited
under the actual caller. A changed claim conflicts. Until then report “claim
retained” and its holder. Current takeover requires a fresh liveness snapshot,
`abandoned_likely`, policy permission and guarded reclaim.

**4. Can coding and grills use different defaults?** Propose optional
`agents.<name>.delegation_defaults` keyed by declared phase. Coding
(`implementation`) could select **6.1 Terra / max**, a grill (`design`)
**6.2 Astra**. Inherit unspecified effort and access. Read-only discovery confirmed this review's explicit
**6.2 Astra / max / Full access**. Codex Auto is
unset in the maintainer's configuration.

Explicit launch values win, then applicable phase preferences, then Agent
defaults. Set Model first, validate dependent effort against the replacement
advertisement and confirm the final tuple before dispatch. An unavailable
requested value keeps the brief unsent. Show phase, effective Model, effort,
access and each value's source in Chat and inspect. Apply presets at creation.
A manual Model or effort choice pins the whole pair in Auto. Preserve
permission policy, advisory phase ranking
(`louiselm-mubf`) and opt-in qualified Auto (`louiselm-economical-routing-1ig31.1`).

### Implementation order and acceptance

1. **Inspect and follow up.** With mock ACP, switch focus and lose an ACK.
   Retry once and require one exported instruction, the right child and intact
   draft. Exercise 55's takeover and 53/76's stable ACP.
2. **Close.** Replay 74's handback and a grill's final confirmation. Compare
   clean disposal with typing during close and a failed disposal fence.
3. **Recover.** Exercise overload with quota left, exhaustion and 128 spent
   IDs. Reconnect the parent and require one actionable status. Use a 46-style
   slot and a changed claim to check exact release, refusal and retained ownership.
4. **Configure defaults and update the source-catalog skill.** Check unset,
   coding, grill, override and unavailable effort on single and batch starts
   against the dispatched turn or unsent brief.

### Operator choices

Choose which assignments close on verified handback, which grills return on
confirmation, the retry budget and approved alternatives, and the phase
preferences. A standing choice covers matching children. With matching
receipts the target is one inspection and one guarded action plus its result,
with zero human relays or manual closes.

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
The maintainer confirmed the original visible prompt, draft/context preservation
and Markdown export in **louiselm-jzw3h** and **louiselm-6twgs.2**.

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
generated API/type/doc checks, and applicable installed gates. The accepted
initial-turn skill exercise and visible prompt/context checks are recorded in
**louiselm-6twgs.2**. The proposed follow-up and close need their own coverage.
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
