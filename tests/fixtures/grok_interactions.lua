-- Official-source fixtures, not captured live frames. Pin: xai-org/grok-build
-- 72a61251fcffb464bcc687aeb5a998e5a98ec0c9, crates/codegen/xai-grok-tools/src/
-- implementations/grok_build/{ask_user_question,exit_plan_mode}/types.rs and
-- ask_user_question/mod.rs. ACP prefixes the method but keeps params flat;
-- grok_ask_user_question.json captures that wire shape from real Grok QA
-- on 2026-10-19 (louiselm-hosvi), proxy Session01a15350-f134-71f0-bd33-d506e3bb2f50,
-- log.jsonl line80 at08:39:41.710Z. Its harmless QA payload is otherwise unchanged.
-- Tests choose synthetic lifecycle ordering; no live ordering is asserted.
return {
  question = {
    sessionId = "grok-acp",
    toolCallId = "question-tool",
    mode = "plan",
    questions = {
      {
        question = "Which database?",
        options = {
          { label = "Redis", description = "In-memory store", preview = "  <div>redis preview</div>\n" },
          { label = "Postgres", description = "Relational DB" },
        },
        multiSelect = false,
      },
      {
        question = "Which features?",
        options = {
          { label = "Fast, simple", description = "A label containing a comma" },
          { label = "Durable", description = "Persist data" },
        },
        multiSelect = true,
      },
      { question = "Anything else?", options = {} },
    },
  },
  plan_review = { sessionId = "grok-acp", toolCallId = "plan-tool", planContent = "# Plan\n\nAdd storage." },
}
