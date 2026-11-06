local MiniTest = require("mini.test")
local InteractionUI = require("louiselm.ui.chat.interactions")
local Interactions = require("louiselm.acp.interactions")
local Fixtures = dofile("tests/fixtures/grok_interactions.lua")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()
local function fake_session(id)
  return {
    inspect = function()
      return { id = id or "session-1", name = id or "session-1", agent = "grok" }
    end,
  }
end

local function harness()
  local picks, inputs, responses, errors = {}, {}, {}, {}
  local prompt = {}
  function prompt:select(items, options, callback)
    assert(not nvim.in_fast_event())
    picks[#picks + 1] = { items = items, options = options, callback = callback }
    return true
  end
  function prompt:input(options, callback)
    assert(not nvim.in_fast_event())
    inputs[#inputs + 1] = { options = options, callback = callback }
    return true
  end
  function prompt:inspect(lines, title, callback)
    self.inspection = { lines = lines, title = title, callback = callback }
    return true
  end
  function prompt:dispose()
    self.inspection = nil
  end
  local cancellations = {}
  local owner = {
    prompt = prompt,
    cancel = function(_, _, ids)
      for _, id in ipairs(ids) do
        cancellations[id]()
      end
    end,
    dispose = function()
      for _, cancel in pairs(cancellations) do
        cancel()
      end
    end,
  }
  MiniTest.finally(function()
    owner:dispose()
  end)
  local original_select, original_input = nvim.ui.select, nvim.ui.input
  MiniTest.finally(function()
    nvim.ui.select, nvim.ui.input = original_select, original_input
  end)
  nvim.ui.select = function()
    error("interaction must use its owned prompt")
  end
  nvim.ui.input = nvim.ui.select
  local function request(session, id, kind)
    local method = "x.ai/" .. (kind == "question" and "ask_user_question" or "exit_plan_mode")
    local data =
      assert(Interactions.parse({ jsonrpc = "2.0", method = method, id = id, params = Fixtures[kind] }, "grok-acp"))
    local identity = session.inspect()
    local label = "[" .. identity.agent .. " " .. identity.id .. "]"
    if identity.name ~= identity.id then
      label = label .. " " .. identity.name:gsub("[\r\n]", " ")
    end
    cancellations[id] = InteractionUI.open(data, function(result)
      responses[#responses + 1] = { id = id, result = result }
      return true
    end, function(message)
      errors[#errors + 1] = message
    end, label, prompt)
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

T["review feedback and preview retain the requesting Session label"] = function()
  local owner, request, picks, inputs, responses, _, choose = harness()
  local first = {
    inspect = function()
      return { id = "session-14", agent = "grok", name = "Abandon QA" }
    end,
  }
  local second = {
    inspect = function()
      return { id = "session-15", agent = "grok", name = "Other\nQA" }
    end,
  }
  local origin = nvim.api.nvim_get_current_buf()
  request(first, "first", "plan_review")
  MiniTest.expect.equality(picks[1].options.prompt, "[grok session-14] Abandon QA · louiselm plan review: ")
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), origin)
  choose("Request changes")
  MiniTest.expect.equality(inputs[1].options.prompt, "[grok session-14] Abandon QA · Plan feedback: ")
  owner:cancel(first, { "first" })
  inputs[1].callback("late feedback")
  MiniTest.expect.equality(responses, {})
  request(second, "second", "plan_review")
  MiniTest.expect.equality(picks[2].options.prompt, "[grok session-15] Other QA · louiselm plan review: ")
  choose("View plan")
  MiniTest.expect.equality(
    owner.prompt.inspection.title,
    "[grok session-15] Other QA · Proposed plan · q/Esc: back to choices"
  )
end

T["questions preserve labels previews multi selections and freeform annotations"] = function()
  local _, request, picks, inputs, responses, _, choose = harness()
  request(fake_session(), "q", "question")
  MiniTest.expect.equality(picks[1].options.prompt, "[grok session-1] · louiselm question 1/3: Which database? ")
  choose("[ ] Redis — In-memory store")
  choose("Next question")
  choose("[ ] Fast, simple — A label containing a comma")
  choose("[ ] Durable — Persist data")
  choose("Next question")
  choose("Other / add notes")
  MiniTest.expect.equality(inputs[1].options.prompt, "[grok session-1] · Answer / notes: ")
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
    request(fake_session(), action, "question")
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
    request(fake_session(), action, "plan_review")
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

T["cancellation while feedback is active retires authority immediately"] = function()
  local owner, request, picks, inputs, responses, _, choose = harness()
  local first, second = fake_session(), fake_session("session-2")
  request(first, "cancelled", "plan_review")
  choose("Request changes")
  owner:cancel(first, { "cancelled" }) -- Core has already sent its one cancellation.
  MiniTest.expect.equality(#picks, 1)
  request(second, "survivor", "plan_review")
  inputs[1].callback("late feedback")
  MiniTest.expect.equality(responses, {})
  MiniTest.expect.equality(#picks, 2)
  inputs[1].callback("repeated feedback")
  choose("Approve")
  MiniTest.expect.equality(responses, { { id = "survivor", result = { outcome = "approved" } } })
end

T["terminal retirement invalidates feedback without sending another response"] = function()
  local owner, request, _, inputs, responses, _, choose = harness()
  request(fake_session(), "active", "plan_review")
  choose("Request changes")
  owner:dispose()
  owner:dispose()
  inputs[1].callback("late feedback")
  MiniTest.expect.equality(responses, {})
end

T["read-only previews close on cancellation without reopening retired questions"] = function()
  local owner, request, picks, _, responses, _, choose = harness()
  local first = fake_session()
  request(first, "preview", "question")
  choose("[ ] Redis — In-memory store")
  choose("View selected preview")
  local preview = owner.prompt.inspection
  MiniTest.expect.equality(preview.lines, { "  <div>redis preview</div>", "" })
  local count = #picks
  owner:cancel(first, { "preview" })
  MiniTest.expect.equality(owner.prompt.inspection, nil)
  preview.callback()
  MiniTest.expect.equality(#picks, count)
  request(fake_session("session-2"), "survivor", "plan_review")
  choose("Approve")
  MiniTest.expect.equality(responses, { { id = "survivor", result = { outcome = "approved" } } })
end

T["fast provider callbacks are scheduled before editor operations"] = function()
  local _, request, picks, _, _, _, choose = harness()
  request(fake_session(), "fast", "question")
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

T["input exceptions cancel once with payload-free errors"] = function()
  local owner, request, picks, _, responses, errors, choose = harness()
  request(fake_session(), "broken", "plan_review")
  owner.prompt.input = function()
    error("private feedback payload")
  end
  choose("Request changes")
  MiniTest.expect.equality(errors, { "interaction UI failed" })
  MiniTest.expect.equality(responses, { { id = "broken", result = { outcome = "cancelled" } } })
  picks[1].callback(picks[1].items[1])
  MiniTest.expect.equality(#responses, 1)
  request(fake_session("session-2"), "next", "plan_review")
  choose("Abandon")
  MiniTest.expect.equality(#responses, 2)
end

T["explicit input refusal cancels with a payload-free presentation error"] = function()
  local owner, request, _, _, responses, errors, choose = harness()
  request(fake_session(), "refused", "plan_review")
  owner.prompt.input = function()
    return false, "private feedback payload"
  end
  choose("Request changes")
  MiniTest.expect.equality(errors, { "interaction UI failed" })
  MiniTest.expect.equality(responses, { { id = "refused", result = { outcome = "cancelled" } } })
end

T["owned question and feedback inputs preserve drafts across hide and show"] = function()
  for _, kind in ipairs({ "question", "plan_review" }) do
    local prompt = require("louiselm.ui.decision_prompt").new()
    local responses = {}
    local method = "x.ai/" .. (kind == "question" and "ask_user_question" or "exit_plan_mode")
    local data =
      assert(Interactions.parse({ jsonrpc = "2.0", method = method, id = kind, params = Fixtures[kind] }, "grok-acp"))
    local cancel = InteractionUI.open(data, function(result)
      responses[#responses + 1] = result
      return true
    end, function(message)
      error(message)
    end, "[grok session-1]", prompt)
    MiniTest.finally(cancel)
    local function submit()
      for _, mapping in ipairs(nvim.api.nvim_buf_get_keymap(prompt.buffer, "n")) do
        if mapping.lhs == "<CR>" then
          mapping.callback()
          return
        end
      end
      error("missing submit mapping")
    end
    local function choose(label)
      for row, line in ipairs(nvim.api.nvim_buf_get_lines(prompt.buffer, 0, -1, false)) do
        if line == label then
          nvim.api.nvim_win_set_cursor(0, { row, 0 })
          submit()
          return
        end
      end
      error("missing choice " .. label)
    end
    if kind == "question" then
      choose("[ ] Redis — In-memory store")
      choose("Other / add notes")
    else
      choose("Request changes")
    end
    local input = prompt.buffer
    nvim.api.nvim_buf_set_lines(input, 0, -1, false, { "Unsubmitted draft" })
    nvim.api.nvim_win_set_cursor(0, { 1, 5 })
    prompt:hide()
    MiniTest.expect.equality(responses, {})
    assert(prompt:show())
    MiniTest.expect.equality(prompt.buffer, input)
    MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(input, 0, -1, false), { "Unsubmitted draft" })
    MiniTest.expect.equality(nvim.api.nvim_win_get_cursor(0), { 1, 5 })
    submit()
    if kind == "question" then
      choose("Next question")
      choose("Next question")
      choose("Submit answers")
      MiniTest.expect.equality(responses[1].answers["Which database?"], { "Redis" })
      MiniTest.expect.equality(responses[1].annotations["Which database?"].notes, "Unsubmitted draft")
    else
      MiniTest.expect.equality(responses, { { outcome = "cancelled", feedback = "Unsubmitted draft" } })
    end
  end
end

return T
