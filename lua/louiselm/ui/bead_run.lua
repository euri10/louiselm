---Presentation and lifecycle of the explicit private-TTY Bead Run view.
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}
local PHASES = {
  authorizing = "Authorizing Run",
  preparing = "Preparing snapshot",
  working = "Working",
  verifying = "Verifying",
  awaiting_promotion = "Waiting for promotion — review the focused window",
  promoting = "Promoting",
}

local function counts(summary)
  local rejected = 0
  for _, failure in ipairs(summary.failed) do
    if failure.outcome == "rejected" then
      rejected = rejected + 1
    end
  end
  local failed = #summary.failed - rejected
  return string.format("Accepted: %d | Rejected: %d | Failed: %d", #summary.accepted, rejected, failed),
    rejected,
    failed
end

---Take ownership of a new controller, install its progress presentation and start.
---Closing the view/editor disposes the controller; a budget hold retains Sessions until q.
---@param controller louiselm.workflow.BeadExecutor New controller; its on_progress callback becomes view-owned.
---@return integer? buffer Run view, or nil if startup was refused.
---@return string? error_message Startup/disposal failure; no worker output.
function M.open(controller)
  if controller.status ~= "new" then
    return nil, "Run view requires a new controller"
  end
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_set_current_buf(buffer)
  nvim.bo[buffer].filetype = "text"
  local finished, disposed, leave = false, false, nil
  local function write(lines)
    if nvim.api.nvim_buf_is_valid(buffer) then
      nvim.bo[buffer].modifiable = true
      nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, nvim.split(table.concat(lines, "\n"), "\n", { plain = true }))
      nvim.bo[buffer].modifiable = false
    end
  end
  local function release()
    if disposed then
      return true
    end
    disposed = true
    if leave then
      nvim.api.nvim_del_autocmd(leave)
      leave = nil
    end
    return controller:dispose()
  end
  local function stop()
    local ok, err = release()
    if not ok then
      nvim.notify(err or "Run disposal failed", nvim.log.levels.ERROR)
    end
    return ok, err
  end
  leave = nvim.api.nvim_create_autocmd("VimLeavePre", { once = true, callback = stop })
  nvim.api.nvim_create_autocmd("BufWipeout", { buffer = buffer, once = true, callback = stop })
  nvim.keymap.set("n", "q", function()
    if disposed then
      nvim.api.nvim_buf_delete(buffer, { force = true })
    else
      local ok, err = stop()
      if finished then
        write({
          ok and "Run stopped by operator" or "Run disposal failed: " .. (err or "unknown outcome"),
          "Press q to close.",
          controller.summary.text,
        })
      end
    end
  end, { buffer = buffer, desc = "Stop Bead Run or close its summary" })
  controller.options.on_progress = function(event)
    if finished or disposed then
      return
    end
    write({
      "Run " .. event.run_id,
      string.format("Bead %d/%d: %s", event.index, event.total, event.bead_id),
      PHASES[event.phase],
      counts(controller.summary),
      "Press q to stop.",
    })
  end
  write({ "Run " .. controller.options.envelope.run_id, "Starting. Press q to stop." })
  local started, start_error = controller:start(function(ok, message, summary)
    finished = true
    local held = controller.status == "held"
    local released, release_error = true, nil
    if not held then
      released, release_error = release()
    end
    local count, rejected, failed = counts(summary)
    local headline
    if not ok then
      headline = (held and "Run held: " or "Run stopped: ") .. (message or "unknown outcome")
    elseif failed > 0 then
      headline = "Run ended with failures"
    elseif rejected > 0 then
      headline = "Run ended with rejections"
    else
      headline = "Run complete"
    end
    local output = { headline, count }
    if held then
      output[#output + 1] = "No successor will start. Press q to dispose retained Sessions."
    end
    if not released then
      output[#output + 1] = release_error or "Run disposal failed"
    end
    output[#output + 1] = summary.text
    output[#output + 1] = "Copy this host handoff before closing. No automatic merge or Beads update."
    write(output)
  end)
  if not started then
    local released, err = release()
    nvim.api.nvim_buf_delete(buffer, { force = true })
    return nil, start_error or (not released and err) or "Run could not start"
  end
  return buffer
end

return M
