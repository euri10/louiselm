local MiniTest = require("mini.test")
local Interactions = require("louiselm.acp.interactions")
local Fixtures = dofile("tests/fixtures/grok_interactions.lua")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function parse(kind, params, wrapped)
  local method = "x.ai/" .. (kind == "question" and "ask_user_question" or "exit_plan_mode")
  return Interactions.parse({
    jsonrpc = "2.0",
    id = 7,
    method = wrapped and "_" .. method or method,
    params = wrapped and { method = method, params = params } or params,
  }, "grok-acp")
end

T["rejects contradictory envelopes and malformed consumed fields"] = function()
  local data, err = Interactions.parse({
    jsonrpc = "2.0",
    id = 1,
    method = "_x.ai/ask_user_question",
    params = { method = "x.ai/exit_plan_mode", params = Fixtures.question },
  }, "grok-acp")
  MiniTest.expect.equality(data, nil)
  MiniTest.expect.equality(assert(err).code, -32602)
  local mutations = {
    function(p)
      p.questions = {}
    end,
    function(p)
      p.questions[2].question = p.questions[1].question
    end,
    function(p)
      p.questions[1].options[2].label = "Redis"
    end,
    function(p)
      p.questions[1].options = { [2] = p.questions[1].options[1], [5] = p.questions[1].options[2] }
    end,
    function(p)
      p.questions[1].options[1].label = ""
    end,
    function(p)
      p.questions[1].options[1].description = false
    end,
    function(p)
      p.questions[1].multiSelect = 0
    end,
  }
  for _, mutate in ipairs(mutations) do
    local params = nvim.deepcopy(Fixtures.question)
    mutate(params)
    data, err = parse("question", params, false)
    MiniTest.expect.equality(data, nil)
    MiniTest.expect.equality(assert(err).code, -32602)
  end
end

T["copies consumed fields accepts null optionals and ignores unknown optional fields"] = function()
  local params = nvim.deepcopy(Fixtures.question)
  params.future = "ignored"
  params.questions[1].options[1].preview = nvim.NIL
  params.questions[1].multiSelect = nvim.NIL
  local data = assert(parse("question", params, true))
  MiniTest.expect.equality(data.questions[1].multi_select, false)
  MiniTest.expect.equality(data.questions[1].options[1].preview, nil)
  params.questions[1].options[1].label = "mutated"
  MiniTest.expect.equality(data.questions[1].options[1].label, "Redis")
  MiniTest.expect.equality(rawget(data, "future"), nil)
  for _, plan in ipairs({
    { sessionId = "grok-acp", toolCallId = "plan" },
    { sessionId = "grok-acp", toolCallId = "plan", planContent = nvim.NIL },
  }) do
    MiniTest.expect.equality(assert(parse("plan_review", plan)).plan_content, nil)
  end
end

T["serializes empty answer maps as objects and omits empty annotations"] = function()
  local data = assert(parse("question", Fixtures.question))
  local response = assert(Interactions.response(data, { outcome = "accepted", answers = {}, annotations = {} }))
  MiniTest.expect.equality(nvim.json.encode(response):find('"answers":{}', 1, true) ~= nil, true)
  MiniTest.expect.equality(response.annotations, nil)
  for _, outcome in ipairs({ "chat_about_this", "skip_interview" }) do
    response = assert(Interactions.response(data, { outcome = outcome }))
    MiniTest.expect.equality(nvim.json.encode(response):find('"partial_answers":{}', 1, true) ~= nil, true)
  end
end

T["refuses invalid answers and contradictory annotations before the wire"] = function()
  local data = assert(parse("question", Fixtures.question))
  local invalid = {
    { outcome = "accepted", answers = { ["unknown?"] = { "Redis" } } },
    { outcome = "accepted", answers = { ["Which database?"] = { "Redis", "Postgres" } } },
    { outcome = "accepted", answers = { ["Which database?"] = "Redis" } },
    { outcome = "accepted", answers = { ["Which features?"] = { "Durable", "Durable" } } },
    { outcome = "accepted", answers = { ["Anything else?"] = { "Other" } } },
    {
      outcome = "accepted",
      answers = { ["Which database?"] = { "Redis" } },
      annotations = { ["Which database?"] = { preview = "trimmed or forged" } },
    },
    {
      outcome = "accepted",
      answers = { ["Which features?"] = { "Durable" } },
      annotations = { ["Which features?"] = { preview = "multi-select preview" } },
    },
    { outcome = "accepted", answers = {}, annotations = { ["Which database?"] = { notes = "unanswered" } } },
    { outcome = "cancelled", answers = {} },
    { outcome = "approved" },
    { outcome = "chat_about_this", partial_answers = { ["Which database?"] = { "Redis" } } },
    { outcome = "chat_about_this", partial_answers = false },
    { outcome = "accepted", answers = { ["Which features?"] = { "Other", "Durable" } } },
  }
  for _, result in ipairs(invalid) do
    local response, err = Interactions.response(data, result)
    MiniTest.expect.equality(response, nil)
    MiniTest.expect.equality(type(err), "string")
  end
  data.mode = "default"
  for _, outcome in ipairs({ "chat_about_this", "skip_interview" }) do
    MiniTest.expect.equality(Interactions.response(data, { outcome = outcome, partial_answers = {} }), nil)
  end
  data = assert(parse("plan_review", Fixtures.plan_review))
  MiniTest.expect.equality(Interactions.response(data, { outcome = "approved", feedback = "contradiction" }), nil)
  MiniTest.expect.equality(Interactions.response(data, { outcome = "cancelled", feedback = false }), nil)
  for _, outcome in ipairs({ "approved", "cancelled", "abandoned" }) do
    MiniTest.expect.equality(assert(Interactions.response(data, { outcome = outcome })), { outcome = outcome })
  end
end

return T
