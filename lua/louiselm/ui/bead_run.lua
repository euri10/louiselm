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

local function track_phone(controller, owner)
  if owner == nil then
    return true
  end
  if
    type(owner) ~= "table"
    or type(owner.track_run) ~= "function"
    or type(owner.untrack_run) ~= "function"
    or type(owner.dispose) ~= "function"
  then
    return false, "Run view requires an explicit phone owner"
  end
  local tracked, err = owner:track_run(controller)
  return tracked, err or (not tracked and "Could not register Run" or nil)
end

local function release_phone(controller, owner)
  if owner == nil then
    return true
  end
  owner:untrack_run(controller)
  return owner:dispose()
end

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

---Own a prepared controller in a waiting view without starting it.
---@param controller louiselm.workflow.BeadExecutor New controller.
---@param identity? { registration_id: string, revision: integer, run_id: string } Exact local registration opened by the owner.
---@param phone_owner? table Explicit phone channel; the view owns it when supplied.
---@return integer? buffer Waiting view, or nil when the controller is not new.
---@return string? error_message
function M.waiting(controller, identity, phone_owner)
  if
    controller.status ~= "new"
    or (
      identity ~= nil
      and (
        type(identity) ~= "table"
        or identity.run_id ~= controller.options.envelope.run_id
        or type(identity.registration_id) ~= "string"
        or type(identity.revision) ~= "number"
        or identity.revision < 1
        or identity.revision % 1 ~= 0
      )
    )
  then
    return nil, "Waiting view requires a new controller"
  end
  local tracked, track_error = track_phone(controller, phone_owner)
  if not tracked then
    return nil, track_error
  end
  local buffer = nvim.api.nvim_create_buf(false, true)
  nvim.api.nvim_set_current_buf(buffer)
  nvim.bo[buffer].filetype = "text"
  local lines = {
    "Run " .. controller.options.envelope.run_id,
    "Waiting; this Run has not started.",
    phone_owner and "Phone controller connecting; availability is not established. Press q to close."
      or "Phone controller availability is not established. Press q to close.",
  }
  if identity ~= nil then
    lines[#lines + 1] = string.format("Registration %s, revision %d", identity.registration_id, identity.revision)
  end
  nvim.api.nvim_buf_set_lines(buffer, 0, -1, false, lines)
  nvim.bo[buffer].modifiable = false
  local disposed, leave = false, nil
  local function release()
    if disposed then
      return
    end
    disposed = true
    if leave then
      nvim.api.nvim_del_autocmd(leave)
      leave = nil
    end
    local stopped, stop_error = release_phone(controller, phone_owner)
    if not stopped then
      nvim.notify(stop_error or "Phone controller disposal failed", nvim.log.levels.ERROR)
    end
    local ok, err = controller:dispose()
    if not ok then
      nvim.notify(err or "Run disposal failed", nvim.log.levels.ERROR)
    end
  end
  leave = nvim.api.nvim_create_autocmd("VimLeavePre", { once = true, callback = release })
  nvim.api.nvim_create_autocmd("BufWipeout", { buffer = buffer, once = true, callback = release })
  nvim.keymap.set("n", "q", function()
    nvim.api.nvim_buf_delete(buffer, { force = true })
  end, { buffer = buffer, desc = "Close waiting Run" })
  return buffer
end

---Take ownership of a new controller, install its progress presentation and start.
---Closing the view/editor disposes the controller; a budget hold retains Sessions until q.
---@param controller louiselm.workflow.BeadExecutor New controller; its on_progress callback becomes view-owned.
---@param phone_owner? table Explicit phone channel; the view owns it when supplied.
---@return integer? buffer Run view, or nil if startup was refused.
---@return string? error_message Startup/disposal failure; no worker output.
function M.open(controller, phone_owner)
  if controller.status ~= "new" then
    return nil, "Run view requires a new controller"
  end
  local tracked, track_error = track_phone(controller, phone_owner)
  if not tracked then
    return nil, track_error
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
    local stopped, stop_error = release_phone(controller, phone_owner)
    local released, release_error = controller:dispose()
    return stopped and released, stop_error or release_error
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
    elseif summary.continuation_required then
      headline = "Saved decision complete; remaining work needs fresh approval"
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
