-- Explicit headless apply consumer; never selects a destination or sources.
---@diagnostic disable-next-line: undefined-global -- Neovim owns this process.
local nvim = vim
local selection_ok, selection = pcall(nvim.json.decode, nvim.env.LOUISELM_USAGE_APPLY_SELECTION or "")
local declaration_ok, declaration = pcall(nvim.json.decode, nvim.env.LOUISELM_USAGE_APPLY_DECLARATION or "")
if not selection_ok or not declaration_ok then
  io.stderr:write("Invalid selection or declaration JSON\n")
  nvim.cmd("cquit 2")
  return
end

local store, store_error = require("louiselm.session.recording").new(
  nvim.env.LOUISELM_USAGE_APPLY_DIRECTORY or "",
  function() end
)
if not store then
  io.stderr:write((store_error and store_error.message or "Invalid import store") .. "\n")
  nvim.cmd("cquit 2")
  return
end
require("louiselm.session.usage_import").apply(store, selection, declaration, function(report, err)
  if err then
    io.stderr:write(err.message .. "\n")
    nvim.cmd("cquit 2")
    return
  end
  io.stdout:write(nvim.json.encode(report) .. "\n")
  nvim.cmd("qa!")
end)
