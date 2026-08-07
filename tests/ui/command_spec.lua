local MiniTest = require("mini.test")

local Command = require("louiselm.ui.chat.command")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function has_chat_command()
  return nvim.api.nvim_get_commands({ builtin = false }).LouiselmChat ~= nil
end

T["command"] = MiniTest.new_set()

T["command"]["minimal init exposes the canonical chat command"] = function()
  MiniTest.expect.equality(has_chat_command(), true)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LouisLMChat, nil)
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LuiseLmChat, nil)
end

T["command"]["manual init exposes the canonical chat command"] = function()
  local original_add = nvim.pack.add
  nvim.pack.add = function() end
  local ok, error_message = pcall(dofile, "manual_init.lua")
  nvim.pack.add = original_add
  MiniTest.expect.equality(ok, true)
  MiniTest.expect.equality(error_message, nil)
  MiniTest.expect.equality(has_chat_command(), true)
end

T["command"]["register is repeatable"] = function()
  local registered = Command.register()
  MiniTest.expect.equality(registered, true)
  MiniTest.expect.equality(has_chat_command(), true)
end

return T
