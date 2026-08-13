local MiniTest = require("mini.test")
local Health = require("louiselm.health")
local Louiselm = require("louiselm")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local T = MiniTest.new_set()

local function with_health_stubs(callback)
  local original_health = nvim.health
  local original_executable = nvim.fn.executable
  local original_system = nvim.system
  local original_in_fast_event = nvim.in_fast_event
  local calls = { ok = {}, error = {}, info = {}, warn = {} }
  nvim.health = {
    start = function(message)
      calls.start = message
    end,
    ok = function(message)
      calls.ok[#calls.ok + 1] = message
    end,
    error = function(message)
      calls.error[#calls.error + 1] = message
    end,
    info = function(message)
      calls.info[#calls.info + 1] = message
    end,
    warn = function(message)
      calls.warn[#calls.warn + 1] = message
    end,
  }
  nvim.fn.executable = function()
    return 1
  end
  nvim.in_fast_event = function()
    return false
  end
  nvim.system = function(command, options, callback)
    callback({ code = 0, signal = 0, stdout = "agent 1.2.3\n", stderr = "" })
    return {}
  end

  local ok, err = pcall(function()
    callback(calls)
  end)

  nvim.health = original_health
  nvim.fn.executable = original_executable
  nvim.system = original_system
  nvim.in_fast_event = original_in_fast_event
  if not ok then
    error(err)
  end
end

T["check"] = MiniTest.new_set()

T["check"]["reports setup validation and agent version"] = function()
  local skill_path = nvim.fn.tempname()
  assert(nvim.fn.mkdir(skill_path, "p") == 1)
  assert(Louiselm.setup({ agents = { agent = { command = "agent" } }, skills = { paths = { skill_path } } }))

  with_health_stubs(function(calls)
    Health.check()
    MiniTest.expect.equality(calls.start, "louiselm")
    MiniTest.expect.equality(calls.error, {})
    MiniTest.expect.equality(calls.ok[1], "configuration is valid")
    MiniTest.expect.equality(calls.ok[2], "agent — agent 1.2.3")
    MiniTest.expect.equality(calls.ok[3], "discovered 0 skills")
    MiniTest.expect.equality(calls.ok[4], "capture recorder is executable: pw-record")
    MiniTest.expect.equality(calls.ok[5], "capture service is executable: louiselm-capture")
  end)

  Health.reset()
  nvim.fn.delete(skill_path, "rf")
end

T["check"]["reports missing setup as a warning"] = function()
  Health.reset()
  with_health_stubs(function(calls)
    MiniTest.expect.equality(Health.check(), false)
    MiniTest.expect.equality(
      calls.warn,
      { "LouiseLM has not been configured; run setup() before checking configuration" }
    )
  end)
end

return T
