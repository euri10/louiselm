local MiniTest = require("mini.test")
local Spawn = require("louiselm.agent.spawn")

local T = MiniTest.new_set()

T["start"] = MiniTest.new_set()

T["start"]["starts a process with an argument array and explicit environment"] = function()
  local calls = {}
  local fake_handle = {}
  ---@diagnostic disable-next-line: undefined-global
  local original_system = vim.system
  ---@diagnostic disable-next-line: undefined-global
  vim.system = function(command, options, on_exit)
    calls.command = command
    calls.options = options
    calls.on_exit = on_exit
    return fake_handle
  end

  local handle, err = Spawn.start({
    command = "claude-agent-acp",
    args = { "--verbose" },
    env = { ANTHROPIC_LOG = "debug" },
  }, function() end)

  ---@diagnostic disable-next-line: undefined-global
  vim.system = original_system

  MiniTest.expect.equality(err, nil)
  MiniTest.expect.equality(handle, fake_handle)
  MiniTest.expect.equality(calls.command, { "claude-agent-acp", "--verbose" })
  MiniTest.expect.equality(calls.options, { text = true, env = { ANTHROPIC_LOG = "debug" } })
  MiniTest.expect.equality(type(calls.on_exit), "function")
end

T["start"]["returns process launch errors instead of throwing"] = function()
  ---@diagnostic disable-next-line: undefined-global
  local original_system = vim.system
  ---@diagnostic disable-next-line: undefined-global
  vim.system = function()
    error("vim.system failed")
  end

  local handle, err = Spawn.start({ command = "agent", args = {} })

  ---@diagnostic disable-next-line: undefined-global
  vim.system = original_system

  MiniTest.expect.equality(handle, nil)
  assert(err ~= nil)
  MiniTest.expect.equality(err:find("vim.system failed", 1, true) ~= nil, true)
end

return T
