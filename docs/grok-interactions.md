# Grok interactive ACP requests

LouiseLM consumes `x.ai/ask_user_question` and `x.ai/exit_plan_mode`, including
the ACP-prefixed methods `_x.ai/ask_user_question` and `_x.ai/exit_plan_mode`.
The underscore changes only the method name: `params` directly contains
`sessionId`, `toolCallId` and the question or plan fields, without an inner
`{method, params}` envelope. These are JSON-RPC methods, distinct from
`_meta.x.ai/sessionConfig.options`.

The contract is pinned to xai-org/grok-build commit
`72a61251fcffb464bcc687aeb5a998e5a98ec0c9`:

- [Question wire types](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-tools/src/implementations/grok_build/ask_user_question/types.rs)
- [Question and option definitions](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-tools/src/implementations/grok_build/ask_user_question/mod.rs)
- [Plan wire types](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-tools/src/implementations/grok_build/exit_plan_mode/types.rs)

The Session validates identity, question/option arrays,
mode, optional booleans/previews and plan content before publishing a typed
`interaction_requested` event. Its data uses snake_case names; its responder
accepts the exact wire result shapes below. Unknown optional peer fields are
ignored. Invalid requests receive `-32602`; unsupported methods receive `-32601`.

| Interaction | Wire result |
| --- | --- |
| Accepted questions | `{outcome="accepted", answers={ [question]={label, ...} }, annotations?={ [question]={preview?, notes?} }}` |
| Plan interview discussion | `{outcome="chat_about_this", partial_answers={ [question]=label }}` |
| Skip plan interview | `{outcome="skip_interview", partial_answers={ [question]=label }}` |
| Cancel questions or plan review | `{outcome="cancelled"}` |
| Approve plan | `{outcome="approved"}` |
| Request plan changes | `{outcome="cancelled", feedback?=text}` |
| Abandon plan | `{outcome="abandoned"}` |

Answer keys are question text. Unanswered questions are omitted. Accepted
answers are arrays, preserving individual labels even when a label contains a
comma. A freeform-only answer is `{"Other"}` with its text in annotation notes.
Single-selection preview annotations preserve the original preview verbatim;
multiple selections never return a preview. Partial answers contain strings:
multiple labels are joined with `", "`, and freeform-only answers become
`"Other"` without notes. The partial outcomes are available only in plan mode.
Empty answer maps encode as JSON objects, and empty annotations are omitted.
ACP-prefixed requests receive these same result objects without another envelope.

A synchronous headless listener may answer during event delivery. An
asynchronous consumer must successfully call `event.claim()` during delivery,
then retain `event.respond` and retire its UI/work on `interaction_cancelled`.
If nobody claims or answers, LouiseLM returns `cancelled` for both kinds. This
deliberately requires an explicit plan approval, unlike Grok's own
[headless client](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-pager/src/headless/ext_protocol.rs),
which approves plans automatically. An observational event listener does not
disable the safe fallback. No extra Agent configuration is needed.

Prompt completion/failure, cancellation, disposal and process exit settle
pending requests once and emit their request IDs in `interaction_cancelled`.
If the process has already exited, settlement still retires callbacks even
though no response can reach the closed transport. Invalid consumer responses
return an error and leave ownership pending; valid responses settle ownership
before writing, including a failed write. LouiseLM owns decision selectors,
inputs and read-only previews, including their suspension and disposal.

Decisions appear only while viewing their requesting Session. A background
request stays pending; it neither opens a picker over another Session nor
blocks that Session's own requests. Switching away hides the decision without
answering it. Returning restores choices, preview position and unfinished
feedback or notes. Cancelling or disposing the requesting Session still
settles its requests exactly once; a hidden or retired presentation cannot
answer through an old callback.

Decision titles identify the requesting Session by Agent, local Session ID and
name. That identity stays on question and plan pickers, feedback/note inputs,
request details and file-edit reviews. This visibility rule applies to ordinary
background Sessions as well as Sessions started by another Session. It does
not change the configured Agent's permission policy or auto-approval choices.

Fixtures in `tests/fixtures/grok_interactions.lua` derive from the official
source above. `tests/fixtures/grok_ask_user_question.json` is a real harmless
questionnaire frame captured on 2026-10-19; it exposed the incorrect extra
envelope assumption tracked in `louiselm-hosvi`. The real plan-review frame in
`tests/fixtures/grok_exit_plan_mode.json` was captured in ACP Session
`01a15376-b52e-73c2-a67b-d51d4f32d95e`, proxy `log.jsonl` line198, during
qa-round `louiselm-r3gec`. Its harmless plan was approved by the maintainer.
Lifecycle race ordering remains synthetic, including libuv delivery races.
Maintainer acceptance for `louiselm-byvq` completed on 2026-11-06: stock-Grok
question and plan actions, cancellation with feedback open, direct disposal
with exercised late callbacks and an independent review, and continuation on
the retained Session. Pinned protocol and human evidence lives in that issue;
automated gates and offline replay alone do not establish live acceptance.
Session-local decision presentation remains separately tracked in `louiselm-d2mgl`.

## Offline UI replay

For human UI checks without Provider access, the existing mock accepts a trusted
captured request through `interaction_file` or `LOUISELM_MOCK_INTERACTION_FILE`.
Each prompt replays that frame, changing only request/Session/tool-call IDs.
It waits for the actual client response, displays that response as JSON, and
ends the turn. Cancel retires the pending request; late responses cannot complete
a later prompt. Unset, the mock keeps its existing behavior. Invalid replay
files fail explicitly instead of falling back to echo.

Start a **separate** Neovim from the checkout, with private state and no shada:

```sh
qa_state=$(mktemp -d /tmp/louiselm-interactions.XXXXXX)
XDG_STATE_HOME="$qa_state" nvim --noplugin -i NONE \
  -u scripts/interaction_qa.lua --listen "$qa_state/nvim.sock"
```

Permission and interaction selectors use Snacks automatically when it is the
configured `vim.ui.select` provider, preserving its select layout, filtering
and key bindings. No extra LouiseLM setting is needed. Without that provider,
they use LouiseLM's native windows; editable inputs and read-only previews
remain native in both cases. LouiseLM owns the decision lifecycle in either
presentation: leaving a Session retains its pending choice and search, and
Snacks' global resume cannot revive a hidden or retired decision. Picker-level
automatic confirmation is disabled for decisions; the Agent's configured
permission policy and auto-approval behavior are unchanged.

Other commands, such as the Session picker, use the configured UI provider.
To check those commands with the usual provider in a separate instance, omit
`--noplugin -u scripts/interaction_qa.lua` and instead add
`-c 'luafile scripts/interaction_qa.lua'`. The init refuses live Sessions; never
source it into an active editor. It configures only `replay-plan`,
`replay-questions`, and `replay-echo`, and starts none of them.

Use `scripts/session-control` with `NVIM` set to that new socket to launch each
explicit case. Keep one fresh operation ID per attempt and record its returned
Session/turn IDs. The human operates the picker; do not drive it with simulated
input. `replay-plan` opens the captured approval plan, `replay-questions` opens
the captured questionnaire, and `replay-echo` checks surviving-Session replies.
No model, credentials, or Provider call is involved. Retain the private state
and operation results as the round's evidence until cleanup is decided.

Replay establishes LouiseLM UI/lifecycle behavior with known peer data. It does
not establish a stock Agent's current behavior or real Grok cancellation timing;
report those acceptance limits separately. The process regressions deliberately
inject late/repeated replies rather than claiming those races were captured.
