local MiniTest = require("mini.test")
local Decisions = require("louiselm.ui.chat.decisions")
local Interactions = require("louiselm.acp.interactions")
local Fixtures = dofile("tests/fixtures/grok_interactions.lua")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local original_select, original_input = nvim.ui.select, nvim.ui.input

local function harness()
  local picks, inputs, responses, errors = {}, {}, {}, {}
  local owner = Decisions.new({
    is_live = function()
      return true
    end,
    report_error = function(_, message)
      errors[#errors + 1] = message
    end,
    resolved = function()
      error("interaction is not a permission")
    end,
  })
  MiniTest.finally(function()
    owner:dispose()
    nvim.ui.select, nvim.ui.input = original_select, original_input
  end)
  nvim.ui.select = function(items, options, callback)
    assert(not nvim.in_fast_event())
    picks[#picks + 1] = { items = items, options = options, callback = callback }
  end
  nvim.ui.input = function(options, callback)
    assert(not nvim.in_fast_event())
    inputs[#inputs + 1] = { options = options, callback = callback }
  end
  local function request(session, id, kind)
    local method = "x.ai/" .. (kind == "question" and "ask_user_question" or "exit_plan_mode")
    local data =
      assert(Interactions.parse({ jsonrpc = "2.0", method = method, id = id, params = Fixtures[kind] }, "grok-acp"))
    owner:request_interaction(session, data, function(result)
      responses[#responses + 1] = { id = id, result = result }
      return true
    end)
  end
  local function choose(label)
    local pick = picks[#picks]
    for _, item in ipairs(pick.items) do
      if pick.options.format_item(item) == label then
        pick.callback(item)
        return
      end
    end
    error("missing choice " .. label)
  end
  return owner, request, picks, inputs, responses, errors, choose
end

T["questions preserve labels previews multi selections and freeform annotations"] = function()
  local _, request, _, inputs, responses, _, choose = harness()
  request({}, "q", "question")
  choose("[ ] Redis — In-memory store")
  choose("Next question")
  choose("[ ] Fast, simple — A label containing a comma")
  choose("[ ] Durable — Persist data")
  choose("Next question")
  choose("Other / add notes")
  inputs[1].callback("Keep it small")
  choose("Submit answers")
  MiniTest.expect.equality(responses, {
    {
      id = "q",
      result = {
        outcome = "accepted",
        answers = {
          ["Which database?"] = { "Redis" },
          ["Which features?"] = { "Fast, simple", "Durable" },
          ["Anything else?"] = { "Other" },
        },
        annotations = {
          ["Which database?"] = { preview = "  <div>redis preview</div>\n" },
          ["Anything else?"] = { notes = "Keep it small" },
        },
      },
    },
  })
end

T["plan interview actions send only label strings in partial_answers"] = function()
  for _, action in ipairs({ "Chat about this", "Skip interview" }) do
    local _, request, _, inputs, responses, _, choose = harness()
    request({}, action, "question")
    choose("[ ] Redis — In-memory store")
    choose("Next question")
    choose("[ ] Fast, simple — A label containing a comma")
    choose("[ ] Durable — Persist data")
    choose("Next question")
    choose("Other / add notes")
    inputs[1].callback("Do not send these notes in a partial")
    choose(action)
    MiniTest.expect.equality(responses[1].result, {
      outcome = action == "Chat about this" and "chat_about_this" or "skip_interview",
      partial_answers = {
        ["Which database?"] = "Redis",
        ["Which features?"] = "Fast, simple, Durable",
        ["Anything else?"] = "Other",
      },
    })
  end
end

T["plan approval feedback and abandonment use exact outcomes"] = function()
  for _, action in ipairs({ "Approve", "Request changes", "Abandon" }) do
    local _, request, _, inputs, responses, _, choose = harness()
    request({}, action, "plan_review")
    choose(action)
    if action == "Request changes" then
      inputs[1].callback("Add error handling")
    end
    MiniTest.expect.equality(
      responses[1].result,
      action == "Approve" and { outcome = "approved" }
        or action == "Abandon" and { outcome = "abandoned" }
        or { outcome = "cancelled", feedback = "Add error handling" }
    )
  end
end

T["cancellation while feedback is active retires authority and isolates a queued Session"] = function()
  local owner, request, picks, inputs, responses, _, choose = harness()
  local first, second = {}, {}
  request(first, "cancelled", "plan_review")
  choose("Request changes")
  request(second, "survivor", "plan_review")
  owner:cancel(first, { "cancelled" }) -- Core has already sent its one cancellation.
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(#picks, 1)
  inputs[1].callback("late feedback")
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(#picks, 2)
  inputs[1].callback("repeated feedback")
  MiniTest.expect.equality(owner:is_active(), true)
  choose("Approve")
  MiniTest.expect.equality(responses, { { id = "survivor", result = { outcome = "approved" } } })
end

T["UI disposal cancels active feedback and queued requests immediately and once"] = function()
  local owner, request, _, inputs, responses, _, choose = harness()
  request({}, "active", "plan_review")
  choose("Request changes")
  request({}, "queued", "question")
  owner:dispose()
  owner:dispose()
  inputs[1].callback("late feedback")
  MiniTest.expect.equality(responses, {
    { id = "active", result = { outcome = "cancelled" } },
    { id = "queued", result = { outcome = "cancelled" } },
  })
end

T["read-only previews close on cancellation without reopening retired questions"] = function()
  local owner, request, picks, _, responses, _, choose = harness()
  local first = {}
  request(first, "preview", "question")
  choose("[ ] Redis — In-memory store")
  choose("View selected preview")
  local window = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_win_get_buf(window)
  MiniTest.expect.equality(nvim.api.nvim_get_option_value("modifiable", { buf = buffer }), false)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), { "  <div>redis preview</div>", "" })
  local count = #picks
  request({}, "survivor", "plan_review")
  owner:cancel(first, { "preview" })
  MiniTest.expect.equality(nvim.api.nvim_win_is_valid(window), false)
  assert(nvim.wait(1000, function()
    return #picks == count + 1
  end, 5))
  choose("Approve")
  MiniTest.expect.equality(responses, { { id = "survivor", result = { outcome = "approved" } } })
end

T["fast provider callbacks are scheduled before editor operations"] = function()
  local _, request, picks, _, _, _, choose = harness()
  request({}, "fast", "question")
  local first = picks[1]
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    assert(nvim.in_fast_event())
    first.callback(first.items[1])
    MiniTest.expect.equality(#picks, 1)
    timer:close()
  end)
  assert(nvim.wait(1000, function()
    return #picks == 2
  end, 5))
  choose("Cancel")
end

T["input exceptions cancel once with payload-free errors and release the shared slot"] = function()
  local owner, request, picks, _, responses, errors, choose = harness()
  request({}, "broken", "plan_review")
  nvim.ui.input = function()
    error("private feedback payload")
  end
  choose("Request changes")
  MiniTest.expect.equality(owner:is_active(), false)
  MiniTest.expect.equality(errors, { "interaction UI failed" })
  MiniTest.expect.equality(responses, { { id = "broken", result = { outcome = "cancelled" } } })
  picks[1].callback(picks[1].items[1])
  MiniTest.expect.equality(#responses, 1)
  request({}, "next", "plan_review")
  choose("Abandon")
  MiniTest.expect.equality(#responses, 2)
end

return T
