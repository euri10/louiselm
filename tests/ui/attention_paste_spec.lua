local MiniTest = require("mini.test")
local AttentionPaste = require("louiselm.ui.attention_paste")

---@diagnostic disable-next-line: undefined-global -- Neovim injects its runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function fixture(previous)
  local original = nvim.paste
  if previous ~= nil then
    nvim.paste = previous
  end
  local owner = AttentionPaste.new()
  MiniTest.finally(function()
    owner:dispose()
    nvim.paste = original
  end)

  local function settle()
    local complete = false
    nvim.schedule(function()
      complete = true
    end)
    assert(nvim.wait(1000, function()
      return complete
    end))
  end

  return { owner = owner, previous = previous or original, settle = settle }
end

local function observer()
  local calls = { count = 0 }
  local value = { disposed = false }
  function value:activity()
    calls.count = calls.count + 1
  end
  ---@cast value louiselm.ui.Attention -- Minimal double for the dispatcher’s disposed/activity contract.
  return value, calls
end

T["dispatcher delegates the cancellation result and preserves predecessor errors"] = function()
  local seen_phase
  local previous_calls = 0
  local should_error = false
  local failure = "fixture paste failure"
  local f = fixture(function(lines, phase)
    previous_calls = previous_calls + 1
    seen_phase = phase
    MiniTest.expect.equality(lines, { "fixture" })
    if should_error then
      error(failure)
    end
    return false
  end)
  local input, calls = observer()
  f.owner:subscribe(input)

  local accepted = nvim.api.nvim_paste("fixture", false, -1)
  f.settle()

  MiniTest.expect.equality(accepted, false)
  MiniTest.expect.equality(seen_phase, -1)
  MiniTest.expect.equality(previous_calls, 1)
  MiniTest.expect.equality(calls.count, 1)

  should_error = true
  local ok, error_message = pcall(nvim.api.nvim_paste, "fixture", false, -1)
  f.settle()

  MiniTest.expect.equality(ok, false)
  MiniTest.expect.equality(tostring(error_message):find(failure, 1, true) ~= nil, true)
  MiniTest.expect.equality(previous_calls, 2)
end

T["one dispatcher serves multiple and sequential subscribers"] = function()
  local f = fixture(function()
    return true
  end)
  local first, first_calls = observer()
  local second, second_calls = observer()
  f.owner:subscribe(first)
  local dispatcher = nvim.paste
  f.owner:subscribe(second)
  MiniTest.expect.equality(nvim.paste, dispatcher)

  MiniTest.expect.equality(nvim.api.nvim_paste("fixture", false, -1), true)
  f.settle()
  MiniTest.expect.equality({ first_calls.count, second_calls.count }, { 1, 1 })

  f.owner:unsubscribe(first)
  MiniTest.expect.equality(nvim.api.nvim_paste("fixture", false, -1), true)
  f.settle()
  MiniTest.expect.equality({ first_calls.count, second_calls.count }, { 1, 2 })

  f.owner:unsubscribe(second)
  MiniTest.expect.equality(nvim.paste, f.previous)
  f.owner:subscribe(second)
  MiniTest.expect.equality(nvim.paste, dispatcher)
  MiniTest.expect.equality(nvim.api.nvim_paste("fixture", false, -1), true)
  f.settle()
  MiniTest.expect.equality(second_calls.count, 3)
  f.owner:unsubscribe(second)
  MiniTest.expect.equality(nvim.paste, f.previous)
end

T["each streamed paste phase counts and delegates unchanged"] = function()
  local phases = {}
  local f = fixture(function(_, phase)
    phases[#phases + 1] = phase
    return true
  end)
  local input, calls = observer()
  f.owner:subscribe(input)

  MiniTest.expect.equality(nvim.api.nvim_paste("start", false, 1), true)
  MiniTest.expect.equality(nvim.api.nvim_paste("middle", false, 2), true)
  MiniTest.expect.equality(nvim.api.nvim_paste("end", false, 3), true)
  f.settle()

  MiniTest.expect.equality(phases, { 1, 2, 3 })
  MiniTest.expect.equality(calls.count, 3)
end

T["a later wrapper can retain the dispatcher across unsubscribe and resubscribe"] = function()
  local previous_calls, later_calls = 0, 0
  local f = fixture(function()
    previous_calls = previous_calls + 1
    return true
  end)
  local first = observer()
  f.owner:subscribe(first)
  local dispatcher = nvim.paste
  local later = function(lines, phase)
    later_calls = later_calls + 1
    return dispatcher(lines, phase)
  end
  nvim.paste = later

  f.owner:unsubscribe(first)
  MiniTest.expect.equality(nvim.paste, later)
  local second, second_calls = observer()
  f.owner:subscribe(second)
  MiniTest.expect.equality(nvim.paste, later)
  MiniTest.expect.equality(nvim.api.nvim_paste("fixture", false, -1), true)
  f.settle()
  MiniTest.expect.equality({ later_calls, previous_calls, second_calls.count }, { 1, 1, 1 })

  nvim.api.nvim_paste("fixture", false, -1)
  f.owner:dispose()
  f.settle()
  MiniTest.expect.equality(nvim.paste, later)
  MiniTest.expect.equality({ later_calls, previous_calls, second_calls.count }, { 2, 2, 1 })
end

T["disposal drops queued activity and releases controller and lines"] = function()
  local weak = setmetatable({}, { __mode = "v" })
  local f = fixture(function(lines)
    weak[2] = lines
    return true
  end)

  local function queue_then_dispose()
    local input, calls = observer()
    weak[1] = input
    f.owner:subscribe(input)
    nvim.api.nvim_paste("fixture", false, -1)
    f.owner:dispose()
    return calls
  end
  local calls = queue_then_dispose()
  collectgarbage("collect")
  f.settle()
  collectgarbage("collect")

  MiniTest.expect.equality(calls.count, 0)
  MiniTest.expect.equality(weak[1], nil)
  MiniTest.expect.equality(weak[2], nil)
end

return T
