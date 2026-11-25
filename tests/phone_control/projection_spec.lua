local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Projection = require("louiselm.phone_control.projection")

local T = MiniTest.new_set()

T["projects only sent conversation and closed interaction fields"] = function()
  local state = { status = "ready" }
  local projection = Projection.new(function()
    return state.status
  end)
  projection:record({ type = "prompt_dispatched", data = { prompt = { { type = "text", text = "sent" } } } })
  projection:record({ type = "chunk", data = { content = { type = "text", text = "answer" } } })
  projection:record({
    type = "tool_call_started",
    data = { toolCallId = "tool-1", title = "Read", status = "in_progress", internal = "hidden" },
  })
  projection:record({
    type = "tool_call_finished",
    data = {
      toolCallId = "tool-1",
      status = "completed",
      content = { { type = "content", content = { type = "text", text = "result" } } },
    },
  })
  projection:record({
    type = "interaction_requested",
    data = {
      kind = "question",
      request_id = 42,
      questions = {
        { question = "Pick?", options = { { label = "One", description = "First" } }, multi_select = false },
      },
    },
  })
  local page = assert(projection:page(0, 50, 0))
  MiniTest.expect.equality(page.entries, {
    { kind = "message", seq = 1, role = "user", text = "sent", truncated = false },
    { kind = "message", seq = 2, role = "assistant", text = "answer", truncated = false },
    {
      kind = "tool",
      seq = 3,
      id = "tool-1",
      title = "Read",
      status = "completed",
      content = "result",
      truncated = false,
    },
    {
      kind = "question",
      seq = 4,
      request_id = "42",
      question = "Pick?",
      options = { { label = "One", description = "First", preview = "" } },
      multi_select = false,
      truncated = false,
    },
  })
  MiniTest.expect.equality(page.complete, true)
  MiniTest.expect.equality(nvim.inspect(page):find("hidden", 1, true), nil)
end

T["projects supported tool input and output without dumping raw objects"] = function()
  local projection = Projection.new(function()
    return "ready"
  end)
  projection:record({
    type = "tool_call_started",
    data = {
      toolCallId = "tool",
      title = "Search",
      status = "in_progress",
      rawInput = { command = "rg needle", private_metadata = "not displayed" },
    },
  })
  projection:record({
    type = "tool_call_finished",
    data = {
      toolCallId = "tool",
      status = "completed",
      rawOutput = { stdout = "found" },
    },
  })
  local page = assert(projection:page(0, 50, 0))
  MiniTest.expect.equality(page.entries[1].content, "Input: command: rg needle\nOutput: stdout: found")
  MiniTest.expect.equality(page.complete, false)
  MiniTest.expect.equality(nvim.inspect(page):find("not displayed", 1, true), nil)
end

T["projects plan reviews and omits invalid tool identities"] = function()
  local projection = Projection.new(function()
    return "waiting_permission"
  end)
  projection:record({
    type = "interaction_requested",
    data = {
      kind = "plan_review",
      request_id = 9,
      plan_content = "Review this plan",
    },
  })
  projection:record({
    type = "tool_call_started",
    data = {
      toolCallId = string.rep("x", 8193),
      title = "invalid",
      status = "in_progress",
    },
  })
  local page = assert(projection:page(0, 50, 0))
  MiniTest.expect.equality(page.entries, {
    { kind = "plan", seq = 1, request_id = "9", text = "Review this plan", truncated = false },
  })
  MiniTest.expect.equality(page.complete, false)
end

T["pages retain revision and reject old cursors or changed snapshots"] = function()
  local projection = Projection.new(function()
    return "ready"
  end)
  for _, text in ipairs({ "one", "two", "three" }) do
    projection:record({ type = "prompt_dispatched", data = { prompt = { { type = "text", text = text } } } })
  end
  local page = assert(projection:page(0, 2, 0))
  MiniTest.expect.equality({ page.entries[1].text, page.entries[2].text, page.next_before }, { "two", "three", 2 })
  local older = assert(projection:page(page.next_before, 2, page.revision))
  MiniTest.expect.equality({ older.entries[1].text, older.next_before }, { "one", 0 })
  projection:record({ type = "chunk", data = { text = "new" } })
  local _, revision_error = projection:page(page.next_before, 2, page.revision)
  MiniTest.expect.equality(revision_error, "stale_subject")
  local _, cursor_error = projection:page(999, 2, 0)
  MiniTest.expect.equality(cursor_error, "stale_subject")
end

T["bounds strings at UTF-8 boundaries"] = function()
  local projection = Projection.new(function()
    return "ready"
  end)
  projection:record({ type = "chunk", data = { text = string.rep("é", 5000) } })
  local page = assert(projection:page(0, 50, 0))
  MiniTest.expect.equality(#page.entries[1].text, 8192)
  MiniTest.expect.equality(page.entries[1].truncated, true)
  MiniTest.expect.equality(page.complete, false)
end

T["rejects invalid UTF-8 and keeps large questions within one frame"] = function()
  local projection = Projection.new(function()
    return "ready"
  end)
  projection:record({ type = "chunk", data = { text = string.char(0xff) } })
  local options = {}
  for index = 1, 50 do
    options[index] = { label = tostring(index), description = string.rep("x", 8192), preview = string.rep("\1", 8192) }
  end
  projection:record({
    type = "interaction_requested",
    data = {
      kind = "question",
      request_id = 1,
      questions = { { question = "Pick", options = options, multi_select = false } },
    },
  })
  local page = assert(projection:page(0, 50, 0))
  MiniTest.expect.equality(page.entries[1].text, "")
  MiniTest.expect.equality(page.entries[1].truncated, true)
  MiniTest.expect.equality(page.entries[2].truncated, true)
  MiniTest.expect.equality(#nvim.json.encode(page) < 262144, true)
  MiniTest.expect.equality(page.complete, false)
end

return T
