# Grok interactive ACP requests

LouiseLM consumes `x.ai/ask_user_question` and `x.ai/exit_plan_mode`, including
the ACP wrapped methods `_x.ai/ask_user_question` and `_x.ai/exit_plan_mode`.
Wrapped `params` contain the matching inner `method` and its `params` object.
These are JSON-RPC methods, distinct from `_meta.x.ai/sessionConfig.options`.

The contract is pinned to xai-org/grok-build commit
`72a61251fcffb464bcc687aeb5a998e5a98ec0c9`:

- [Question wire types](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-tools/src/implementations/grok_build/ask_user_question/types.rs)
- [Question and option definitions](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-tools/src/implementations/grok_build/ask_user_question/mod.rs)
- [Plan wire types](https://github.com/xai-org/grok-build/blob/72a61251fcffb464bcc687aeb5a998e5a98ec0c9/crates/codegen/xai-grok-tools/src/implementations/grok_build/exit_plan_mode/types.rs)

The Session validates identity, envelope agreement, question/option arrays,
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
Wrapped requests receive these same result objects without another envelope.

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
before writing, including a failed write. UI code owns selectors, inputs and
read-only preview windows. A retired provider-owned selector/input may remain
visible until dismissed; it cannot authorize a response or release a newer
Session's decision slot.

Fixtures in `tests/fixtures/grok_interactions.lua` derive from the official
source above. Their lifecycle order is synthetic, including libuv delivery
races; it is not a captured Grok run. `louiselm-byvq` remains open for maintainer
acceptance when Grok quota permits: exercise single/multiple/freeform answers,
both plan interview actions, approve/change/abandon review, cancel while
feedback input is active, and dispose with late callbacks. Confirm continuation
on a surviving Session. Automated gates do not establish that live acceptance.
