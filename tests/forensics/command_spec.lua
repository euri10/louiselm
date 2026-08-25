local MiniTest = require("mini.test")
local Protocol = require("louiselm.acp.protocol")
local LogView = require("louiselm.forensics.log_view")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      nvim.cmd("new")
    end,
    post_case = function()
      nvim.cmd("bwipeout!")
    end,
  },
})

T["registers :LouiselmForensicsLogView"] = function()
  MiniTest.expect.equality(nvim.api.nvim_get_commands({ builtin = false }).LouiselmForensicsLogView ~= nil, true)
end

T[":LouiselmForensicsLogView toggles folded rendering for the current window"] = function()
  local win = nvim.api.nvim_get_current_win()
  local buffer = nvim.api.nvim_get_current_buf()
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { assert(Protocol.encode(Protocol.response(1, { ok = true }))) })

  MiniTest.expect.equality(LogView.is_enabled(win), false)
  nvim.cmd("LouiselmForensicsLogView")
  MiniTest.expect.equality(LogView.is_enabled(win), true)
  nvim.cmd("LouiselmForensicsLogView")
  MiniTest.expect.equality(LogView.is_enabled(win), false)
end

return T
