local LogView = require("louiselm.forensics.log_view")

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---Register the forensics log-view command. No configuration surface: this is
---an opt-in toggle for the current window, not a persistent setting.
function M.register()
  nvim.api.nvim_create_user_command("LouiselmForensicsLogView", function()
    LogView.toggle()
  end, {
    desc = "Toggle folded, human-readable rendering of an ACP JSON-RPC log buffer",
    force = true,
  })
end

return M
