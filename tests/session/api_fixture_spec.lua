local MiniTest = require("mini.test")
local T = MiniTest.new_set()
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local function probe(missing_sqlite)
  local process = nvim.system({
    nvim.v.progpath,
    "--headless",
    "--noplugin",
    "-u",
    "tests/minimal_init.lua",
    "-c",
    "lua dofile('tests/fixtures/session_api_isolation.lua')(" .. tostring(missing_sqlite) .. ")",
  }, { text = true, timeout = 20000 })
  MiniTest.finally(function()
    if not process:is_closing() then
      process:kill(9)
      process:wait()
    end
  end)
  local result = process:wait()
  assert(result.code == 0, result.stdout .. result.stderr)
end

T["API cases ignore a prior case's recording lock"] = function()
  probe(false)
end

T["failed API admission releases its Session and process fixture before replay"] = function()
  probe(true)
end

return T
