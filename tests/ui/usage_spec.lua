local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function page(rows, next_offset)
  return {
    timezone = "UTC",
    view = "summary",
    total = 2,
    next_offset = next_offset,
    mixed_turns = 1,
    excluded_mixed = 1,
    summary = {
      turns = 2,
      outcomes = { completed = 2 },
      tokens = { total_tokens = { samples = 1, average = 10, total = 10 } },
      costs = {},
    },
    rows = rows or {},
  }
end
local function open()
  local calls = {}
  local view = assert(require("louiselm.ui.usage").open({
    query = function(query, callback)
      local call = { query = query, callback = callback, cancelled = false }
      calls[#calls + 1] = call
      return function()
        call.cancelled = true
      end
    end,
  }))
  MiniTest.finally(function()
    view:dispose()
  end)
  return view, calls
end
local function text(view)
  return table.concat(nvim.api.nvim_buf_get_lines(view.buffer, 0, -1, false), "\n")
end
local function deliver(call, value, err)
  local fired = false
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    assert(nvim.in_fast_event())
    call.callback(value, err)
    fired = true
    timer:close()
  end)
  assert(nvim.wait(1000, function()
    return fired
  end, 1))
  nvim.wait(10, function()
    return false
  end, 1)
end

T["view recomputes filters pages and ignores superseded fast-event results"] = function()
  local view, calls = open()
  MiniTest.expect.equality(text(view):find("Loading", 1, true) ~= nil, true)
  view:set_query({ filters = { agent = "codex", ["option:enabled"] = false }, bucket = "day", limit = 1 })
  MiniTest.expect.equality(calls[2].query.filters["option:enabled"], false)
  MiniTest.expect.equality(calls[1].cancelled, true)
  deliver(calls[2], page({}, 1))
  MiniTest.expect.equality(text(view):find("coverage 1/2", 1, true) ~= nil, true)
  MiniTest.expect.equality(text(view):find("excluded", 1, true) ~= nil, true)
  local current = text(view)
  deliver(calls[1], nil, { code = "storage", message = "old failure" })
  MiniTest.expect.equality(text(view), current)
  view:next_page()
  MiniTest.expect.equality(calls[3].query.offset, 1)
  deliver(calls[3], page())
  view:previous_page()
  MiniTest.expect.equality(calls[4].query.offset, 0)
end

T["group and turn navigation preserve query and show starting state and source"] = function()
  local view, calls = open()
  local row = {
    dimensions = { agent = "codex", model = "astra" },
    bucket_start = "2026-09-07T00:00:00Z",
    bucket_end = "2026-09-08T00:00:00Z",
    summary = { turns = 1, outcomes = {}, tokens = {}, costs = {} },
  }
  deliver(calls[1], page({ row }))
  view:enter(1)
  MiniTest.expect.equality(calls[2].query.view, "turns")
  MiniTest.expect.equality(calls[2].query.filters.model, "astra")
  MiniTest.expect.equality(calls[2].query.until_time, row.bucket_end)
  local turn = {
    id = "turn",
    agent = "codex",
    acp_session_id = "session",
    provider = "OpenAI",
    model = "astra",
    options = { enabled = false },
    prepared_at = row.bucket_start,
    mixed = true,
    summary = row.summary,
  }
  deliver(calls[2], page({ turn }))
  view:enter(1)
  MiniTest.expect.equality(calls[3].query.turn_id, "turn")
  deliver(
    calls[3],
    page({
      {
        kind = "options",
        observed_at = row.bucket_start,
        sequence = 1,
        data = {
          source = "response",
          request = { id = 7, option = "model", value = "terra" },
          previous_options = { model = "astra" },
          options = { model = "terra" },
        },
      },
    })
  )
  MiniTest.expect.equality(text(view):find("Starting options", 1, true) ~= nil, true)
  MiniTest.expect.equality(text(view):find("response", 1, true) ~= nil, true)
  MiniTest.expect.equality(text(view):find('"enabled":false', 1, true) ~= nil, true)
  view:back()
  MiniTest.expect.equality(calls[4].query.view, "turns")
  view:back()
  MiniTest.expect.equality(calls[5].query.view, nil)
end

T["empty errors retry and wipe invalidate queued delivery"] = function()
  local view, calls = open()
  local empty = page()
  empty.total, empty.summary.turns = 0, 0
  deliver(calls[1], empty)
  MiniTest.expect.equality(text(view):find("No matching history", 1, true) ~= nil, true)
  view:refresh()
  deliver(calls[2], nil, { code = "invalid", message = "invalid usage query" })
  MiniTest.expect.equality(text(view):find("invalid usage query", 1, true) ~= nil, true)
  view:refresh()
  local buffer = view.buffer
  nvim.api.nvim_buf_delete(buffer, { force = true })
  MiniTest.expect.equality(calls[3].cancelled, true)
  deliver(calls[3], page())
  MiniTest.expect.equality(view.disposed, true)
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
end

T["bucket drilldown keeps fractional range limits and dimension keys apply typed values"] = function()
  local view, calls = open()
  view:set_query({ from = "2026-09-07T00:00:00.500Z", until_time = "2026-09-07T00:00:00.900Z", bucket = "hour" })
  deliver(
    calls[2],
    page({ { dimensions = {}, bucket_start = "2026-09-07T00:00:00Z", bucket_end = "2026-09-07T01:00:00Z" } })
  )
  view:enter(1)
  MiniTest.expect.equality(calls[3].query.from, "2026-09-07T00:00:00.500Z")
  MiniTest.expect.equality(calls[3].query.until_time, "2026-09-07T00:00:00.900Z")
  deliver(calls[3], page())
  nvim.api.nvim_feedkeys("d", "mx!", false)
  MiniTest.expect.equality(calls[4].query.view, "dimensions")
  deliver(calls[4], page({ { dimension = "option:enabled", value = false } }))
  view:enter(1)
  MiniTest.expect.equality(calls[5].query.filters["option:enabled"], false)
  deliver(calls[5], page())
  nvim.api.nvim_feedkeys("b", "mx!", false)
  MiniTest.expect.equality(calls[6].query.bucket, "day")
end

T["command opens history with no Agent configuration and re-registration disposes it"] = function()
  local previous_state = nvim.env.XDG_STATE_HOME
  local state = nvim.fn.tempname()
  nvim.env.XDG_STATE_HOME = state
  MiniTest.finally(function()
    nvim.env.XDG_STATE_HOME = previous_state
    nvim.fn.delete(state, "rf")
  end)
  local Command = require("louiselm.ui.chat.command")
  Command.register()
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LouiselmUsage ~= nil, true)
  nvim.api.nvim_cmd({ cmd = "LouiselmUsage" }, {})
  local buffer = nvim.api.nvim_get_current_buf()
  MiniTest.finally(function()
    Command.register()
  end)
  assert(nvim.wait(1000, function()
    return table.concat(nvim.api.nvim_buf_get_lines(buffer, 0, -1, false), "\n"):find("No matching history", 1, true)
      ~= nil
  end, 1))
  Command.register()
  MiniTest.expect.equality(nvim.api.nvim_buf_is_valid(buffer), false)
end
return T
