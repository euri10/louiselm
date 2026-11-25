-- Explicit local entrypoint; the owner loads only the selected published revision.
---@diagnostic disable-next-line: undefined-global -- Neovim operator script.
local nvim = vim
local function refuse(message)
  nvim.notify(message, nvim.log.levels.ERROR)
end

local leave
local pending, err = require("louiselm.workflow.prepared").open_registered(
  nvim.g.louiselm_bead_run_registration,
  nvim.g.louiselm_bead_run_revision,
  function(controller, identity, error_message)
    if leave then
      nvim.api.nvim_del_autocmd(leave)
      leave = nil
    end
    if not controller then
      return refuse(error_message or "Registered Run is unavailable")
    end
    local buffer, view_error = require("louiselm.ui.bead_run").waiting(controller, identity)
    if not buffer then
      local disposed, dispose_error = controller:dispose()
      refuse(not disposed and dispose_error or view_error or "Waiting Run view is unavailable")
    end
  end
)
if not pending then
  return refuse(err or "Registered Run is unavailable")
end
leave = nvim.api.nvim_create_autocmd("VimLeavePre", {
  once = true,
  callback = function()
    local cancelled, cancel_error = pending:cancel()
    if not cancelled then
      refuse(cancel_error or "Could not cancel registered Run request")
    end
  end,
})
