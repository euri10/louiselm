local MiniTest = require("mini.test")
local Health = require("louiselm.agent.health")

local T = MiniTest.new_set()

local function with_stubs(executable, system, callback)
  ---@diagnostic disable-next-line: undefined-global
  local original_executable = vim.fn.executable
  ---@diagnostic disable-next-line: undefined-global
  local original_system = vim.system
  ---@diagnostic disable-next-line: undefined-global
  vim.fn.executable = executable
  ---@diagnostic disable-next-line: undefined-global
  vim.system = system
  local ok, err = pcall(callback)
  ---@diagnostic disable-next-line: undefined-global
  vim.fn.executable = original_executable
  ---@diagnostic disable-next-line: undefined-global
  vim.system = original_system
  if not ok then
    error(err)
  end
end

T["check"] = MiniTest.new_set()

T["check"]["reports executable availability and detected version"] = function()
  local calls = {}
  local report
  local fake_handle = {}

  with_stubs(function(command)
    calls.executable = command
    return 1
  end, function(command, options, on_exit)
    calls.command = command
    calls.options = options
    on_exit({ code = 0, signal = 0, stdout = "claude-agent-acp 1.2.3\n", stderr = "" })
    return fake_handle
  end, function()
    local handle, err = Health.check({ command = "claude-agent-acp", args = {} }, function(result)
      report = result
    end)

    MiniTest.expect.equality(err, nil)
    MiniTest.expect.equality(handle, fake_handle)
  end)

  MiniTest.expect.equality(calls.executable, "claude-agent-acp")
  MiniTest.expect.equality(calls.command, { "claude-agent-acp", "--version" })
  MiniTest.expect.equality(calls.options, { text = true })
  MiniTest.expect.equality(report.ok, true)
  MiniTest.expect.equality(report.available, true)
  MiniTest.expect.equality(report.version, "claude-agent-acp 1.2.3")
end

T["check"]["reports a missing executable without spawning"] = function()
  local process_started = false
  local report

  with_stubs(function()
    return 0
  end, function()
    process_started = true
    return {}
  end, function()
    local handle, err = Health.check({ command = "missing-agent", args = {} }, function(result)
      report = result
    end)

    MiniTest.expect.equality(handle, nil)
    MiniTest.expect.equality(err, "missing-agent: executable not found on PATH")
  end)

  MiniTest.expect.equality(process_started, false)
  MiniTest.expect.equality(report.ok, false)
  MiniTest.expect.equality(report.available, false)
  MiniTest.expect.equality(report.error, "executable not found on PATH")
end

return T
