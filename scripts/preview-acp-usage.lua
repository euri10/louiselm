-- Entrypoint for the isolated headless report consumer.
---@diagnostic disable-next-line: undefined-global -- Neovim owns this process.
local nvim = vim
local ok, selection = pcall(nvim.json.decode, nvim.env.LOUISELM_USAGE_PREVIEW_SELECTION or "")
if not ok then
  io.stderr:write("Invalid selection JSON\n")
  nvim.cmd("cquit 2")
  return
end

require("louiselm.session.usage_import").preview(selection, function(report, err)
  if err then
    io.stderr:write(err.message .. "\n")
    nvim.cmd("cquit 2")
    return
  end
  io.stdout:write(nvim.json.encode(report) .. "\n")
  nvim.cmd("qa!")
end)
