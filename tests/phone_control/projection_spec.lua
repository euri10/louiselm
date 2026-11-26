local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Projection = require("louiselm.phone_control.projection")

local T = MiniTest.new_set()

T["projects only committed queue display fields"] = function()
  local state = {
    revision = 3,
    paused = true,
    uncertain = false,
    entries = {
      { id = "first", display_text = "first accepted", prompt = "SECRET PROMPT", contexts = { "SECRET CONTEXT" } },
      { id = "second", display_text = "second accepted", metadata = { private = "SECRET ROUTING" } },
    },
    uncertain_proposal = { entries = { { display_text = "SECRET UNCONFIRMED" } } },
  }
  local queue = Projection.queue(state)
  MiniTest.expect.equality(queue.entries, {
    { id = "first", text = "first accepted", truncated = false },
    { id = "second", text = "second accepted", truncated = false },
  })
  MiniTest.expect.equality(queue.paused, true)
  MiniTest.expect.equality(nvim.json.encode(queue):find("SECRET", 1, true), nil)
end

T["bounds queue text and preserves distinct active replacement and uncertainty states"] = function()
  local long = string.rep("é", 400)
  local state = {
    revision = 9,
    paused = true,
    uncertain = true,
    entries = { { id = "first", display_text = long } },
    active = { id = "active", display_text = "running" },
    active_status = "dispatched",
    replacement = {
      entry = { id = "reserve", display_text = "replacement" },
      status = "unsent",
      cancellation = "unknown",
    },
  }
  local queue = Projection.queue(state)
  MiniTest.expect.equality(queue.uncertain, true)
  MiniTest.expect.equality(queue.active.status, "dispatched")
  MiniTest.expect.equality(queue.replacement.status, "unsent")
  MiniTest.expect.equality(queue.replacement.cancellation, "unknown")
  MiniTest.expect.equality(queue.entries[1].truncated, true)
  MiniTest.expect.equality(long:sub(1, #queue.entries[1].text), queue.entries[1].text)
  MiniTest.expect.equality(#nvim.json.encode({ text = queue.entries[1].text }) <= 512, true)
  MiniTest.expect.equality(nvim.json.decode(nvim.json.encode(queue)).entries[1].text, queue.entries[1].text)
end

T["bounds a full queue within the remaining phone frame"] = function()
  local entries = {}
  for index = 1, 20 do
    entries[index] = { id = string.format("%032x", index), display_text = string.rep("\1", 8192) }
  end
  local projected = Projection.queue({
    revision = 20,
    paused = true,
    uncertain = false,
    entries = entries,
    active = { id = string.rep("a", 32), display_text = string.rep("\1", 8192), prompt = "", contexts = {} },
    active_status = "dispatched",
    replacement = {
      entry = { id = string.rep("b", 32), display_text = string.rep("\1", 8192), prompt = "", contexts = {} },
      target_turn_id = "turn",
      status = "unsent",
      cancellation = "unknown",
    },
  })
  MiniTest.expect.equality(#projected.entries, 20)
  MiniTest.expect.equality(projected.entries[1].truncated, true)
  MiniTest.expect.equality(#nvim.json.encode(projected) < 14000, true)
end

T["projects only confirmed model effort and access labels and invalidates old snapshots"] = function()
  local options = {
    {
      id = "model",
      category = "model",
      type = "select",
      current_value = "m1",
      options = {
        { value = "m1", name = "Model One" },
        { value = "m2", name = "Model Two" },
      },
    },
    {
      id = "effort",
      category = "thought_level",
      type = "select",
      current_value = "high",
      options = {
        { value = "high", name = "High" },
      },
    },
    {
      id = "access",
      category = "mode",
      type = "select",
      current_value = "full",
      options = {
        { value = "full", name = "Full access" },
      },
    },
    {
      id = "private",
      category = "other",
      type = "select",
      current_value = "secret",
      options = {
        { value = "secret", name = "PRIVATE CONFIG" },
      },
    },
  }
  local projection = Projection.new(function()
    return "ready"
  end, options)
  local first = assert(projection:page(0, 10, 0))
  MiniTest.expect.equality(first.settings, { model = "Model One", effort = "High", mode = "Full access" })
  MiniTest.expect.equality(nvim.inspect(first):find("PRIVATE CONFIG", 1, true), nil)
  options[1].current_value = "m2"
  MiniTest.expect.equality(assert(projection:page(0, 10, 0)).settings.model, "Model One")
  projection:record({ type = "config_options_changed", data = options })
  local _, err = projection:page(0, 10, first.revision)
  MiniTest.expect.equality(err, "stale_subject")
  MiniTest.expect.equality(assert(projection:page(0, 10, 0)).settings.model, "Model Two")
  projection:record({
    type = "config_options_changed",
    data = {
      {
        id = "model",
        category = "model",
        type = "select",
        current_value = "m",
        options = {
          { value = "m", name = "Other model" },
        },
      },
    },
  })
  MiniTest.expect.equality(assert(projection:page(0, 10, 0)).settings, {
    model = "Other model",
    effort = "",
    mode = "",
  })
end

T["omits ambiguous invalid and oversized setting labels"] = function()
  local projection = Projection.new(function()
    return "ready"
  end, {
    {
      id = "a",
      name = "Model",
      category = "model",
      type = "select",
      current_value = "a",
      options = { { value = "a", name = "one" } },
    },
    {
      id = "b",
      name = "Model",
      category = "model",
      type = "select",
      current_value = "b",
      options = { { value = "b", name = "two" } },
    },
    {
      id = "effort",
      name = "Effort",
      category = "thought_level",
      type = "select",
      current_value = "a",
      options = {
        { value = "a", name = string.rep("x", 257) },
      },
    },
    {
      id = "mode",
      name = "Mode",
      category = "mode",
      type = "select",
      current_value = "a",
      options = { { value = "a", name = "bad\0mode" } },
    },
  })
  MiniTest.expect.equality(assert(projection:page(0, 10, 0)).settings, { model = "", effort = "", mode = "" })
end

T["reports an advertised boolean option without inventing a mode"] = function()
  local projection = Projection.new(function()
    return "ready"
  end, {
    { id = "mode", category = "mode", name = "Full access", type = "boolean", current_value = false },
  })
  MiniTest.expect.equality(assert(projection:page(0, 10, 0)).settings.mode, "Full access: false")
end

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
