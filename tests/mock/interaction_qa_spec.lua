local MiniTest = require("mini.test")
local T = MiniTest.new_set()
---@diagnostic disable-next-line: undefined-global -- Neovim injects the test runtime.
local nvim = vim

T["isolated replay setup advertises only offline Agents and starts no Sessions"] = function()
  local state = nvim.fn.tempname()
  MiniTest.finally(function()
    nvim.fn.delete(state, "rf")
  end)
  local result = nvim
    .system({
      nvim.v.progpath,
      "--headless",
      "--noplugin",
      "-i",
      "NONE",
      "-u",
      "scripts/interaction_qa.lua",
      "-c",
      "lua local d,e=require('louiselm').session_control({action='discover'}); assert(d,e); io.stdout:write(vim.json.encode(d)..'\\n')",
      "-c",
      "qa!",
    }, { cwd = nvim.fn.getcwd(), env = { XDG_STATE_HOME = state }, text = true })
    :wait(5000)
  MiniTest.expect.equality(result.code, 0)
  assert(result.stdout ~= "", result.stderr)
  local discovery = nvim.json.decode(result.stdout)
  MiniTest.expect.equality(discovery.agents, { "replay-echo", "replay-plan", "replay-questions" })
  MiniTest.expect.equality(discovery.sessions, {})
end

return T
