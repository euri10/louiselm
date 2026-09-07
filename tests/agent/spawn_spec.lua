local MiniTest = require("mini.test")
local Spawn = require("louiselm.agent.spawn")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@param system function
local function set_system(system)
  rawset(nvim, "system", system)
end

T["start"] = MiniTest.new_set()

T["start"]["starts a process with an argument array and explicit environment"] = function()
  local calls = {}
  local fake_handle = {}
  local original_system = nvim.system
  set_system(function(command, options, on_exit)
    calls.command = command
    calls.options = options
    calls.on_exit = on_exit
    return fake_handle
  end)

  local handle, err = Spawn.start({
    provider = "test-service",
    command = "claude-agent-acp",
    args = { "--verbose" },
    env = { ANTHROPIC_LOG = "debug" },
  }, function() end)

  set_system(original_system)

  MiniTest.expect.equality(err, nil)
  MiniTest.expect.equality(handle, fake_handle)
  MiniTest.expect.equality(calls.command, { "claude-agent-acp", "--verbose" })
  MiniTest.expect.equality(calls.options, { text = true, env = { ANTHROPIC_LOG = "debug" } })
  MiniTest.expect.equality(type(calls.on_exit), "function")
end

T["start"]["returns process launch errors instead of throwing"] = function()
  local original_system = nvim.system
  set_system(function()
    error("vim.system failed")
  end)

  local handle, err = Spawn.start({ provider = "test-service", command = "agent", args = {} })

  set_system(original_system)

  MiniTest.expect.equality(handle, nil)
  assert(err ~= nil)
  MiniTest.expect.equality(err:find("vim.system failed", 1, true) ~= nil, true)
end

return T
