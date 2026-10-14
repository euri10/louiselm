-- Official-source fixtures, not captured live frames. Pin: xai-org/grok-build
-- 72a61251fcffb464bcc687aeb5a998e5a98ec0c9, crates/codegen/xai-grok-tools/src/
-- implementations/grok_build/{ask_user_question,exit_plan_mode}/types.rs and
-- ask_user_question/mod.rs; wrapped envelope: xai-grok-shell/src/leader/server.rs.
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
