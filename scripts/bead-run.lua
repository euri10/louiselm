-- Explicit private-TTY operator entrypoint; never sourced by a worker.
---@diagnostic disable-next-line: undefined-global -- Neovim operator script.
local nvim = vim
local function refuse(message)
  nvim.notify(message, nvim.log.levels.ERROR)
end
local path = nvim.g.louiselm_bead_run_selection
local metadata = type(path) == "string" and nvim.uv.fs_stat(path) or nil
if
  not metadata
  or metadata.type ~= "file"
  or metadata.size > 65536
  or metadata.uid ~= nvim.uv.os_getuid()
  or metadata.mode % 512 ~= 384
then
  return refuse("Run selection must be an operator-owned mode-0600 JSON file, at most 64KiB")
end
local read, lines = pcall(nvim.fn.readfile, path)
if not read then
  return refuse("Run selection is unavailable")
end
local decoded, selection = pcall(nvim.json.decode, table.concat(lines, "\n"))
if not decoded then
  return refuse("Run selection is invalid JSON")
end
local controller, err = require("louiselm.workflow.operator").new(selection)
if not controller then
  return refuse(err or "Run selection refused")
end
local buffer = nvim.api.nvim_create_buf(false, true)
nvim.api.nvim_set_current_buf(buffer)
nvim.bo[buffer].filetype = "text"
nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, { "Run " .. selection.envelope.run_id, "Starting. Press q to stop." })
local leave
leave = nvim.api.nvim_create_autocmd("VimLeavePre", {
  once = true,
  callback = function()
    leave = nil
    local ok, dispose_error = controller:dispose()
    if not ok then
      nvim.notify(dispose_error or "Run disposal failed", nvim.log.levels.ERROR)
    end
  end,
})
nvim.keymap.set("n", "q", function()
  if controller.status == "disposed" then
    nvim.api.nvim_buf_delete(buffer, { force = true })
    return
  end
  local ok, dispose_error = controller:dispose()
  if not ok then
    nvim.notify(dispose_error or "Run disposal failed", nvim.log.levels.ERROR)
  end
end, { buffer = buffer, desc = "Stop Bead Run or close its summary" })
local started, start_error = controller:start(function(ok, message, summary)
  local disposed, dispose_error = controller:dispose()
  if leave then
    nvim.api.nvim_del_autocmd(leave)
    leave = nil
  end
  local output = { ok and "Run complete" or "Run stopped: " .. (message or "unknown outcome") }
  if not disposed then
    output[#output + 1] = dispose_error or "Run disposal failed"
  end
  output[#output + 1] = summary.text
  output[#output + 1] = "Copy this host handoff before closing. No automatic merge or Beads update."
  if nvim.api.nvim_buf_is_valid(buffer) then
    nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, nvim.split(table.concat(output, "\n"), "\n", { plain = true }))
  end
end)
if not started then
  local disposed, dispose_error = controller:dispose()
  if leave then
    nvim.api.nvim_del_autocmd(leave)
    leave = nil
  end
  return refuse(start_error or (not disposed and dispose_error) or "Run could not start")
end
