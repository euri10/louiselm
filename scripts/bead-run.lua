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
  or metadata.uid ~= nvim.uv.getuid()
  or metadata.mode % 512 ~= 384
then
  return refuse("Run selection must be an operator-owned mode-0600 JSON file, at most 64KiB")
end
local read, lines = pcall(nvim.fn.readfile, path, "b")
if not read then
  return refuse("Run selection is unavailable")
end
local selection_bytes = table.concat(lines, "\n")
local decoded, selection = pcall(nvim.json.decode, selection_bytes)
if not decoded then
  return refuse("Run selection is invalid JSON")
end
local function launch(controller, phone_owner)
  local function release()
    local stopped, stop_error = true, nil
    if phone_owner then
      stopped, stop_error = phone_owner:dispose()
    end
    local disposed, dispose_error = controller:dispose()
    return stopped and disposed, stop_error or dispose_error
  end
  local module = nvim.api.nvim_get_runtime_file("lua/louiselm/workflow/operator.lua", false)[1]
  if not module then
    local disposed, dispose_error = release()
    if not disposed then
      return refuse(dispose_error or "Run disposal failed")
    end
    return refuse("Trusted LouiseLM plugin is unavailable")
  end
  local plugin = nvim.fs.dirname(nvim.fs.dirname(nvim.fs.dirname(nvim.fs.dirname(module))))
  local pending
  local function check(callback)
    -- The controller owns the selection parsed above, not later edits of its file.
    local private =
      string.format("%s/rehearsal-check-%d-%.0f.json", selection.snapshot_parent, nvim.uv.os_getpid(), nvim.uv.hrtime())
    local written, write_error =
      require("louiselm.private_file").write(nvim.uv, private, selection_bytes, "Run readiness selection", "publish")
    if not written then
      return false, write_error
    end
    local started, job = pcall(nvim.system, {
      "/usr/local/lib/louiselm/current/bin/louiselm-control",
      "run",
      "rehearsal-check",
      "--selection",
      private,
      "--plugin",
      plugin,
      "--json",
    }, { text = true, timeout = 120000 }, function(result)
      nvim.schedule(function()
        pending = nil
        local removed, remove_error = nvim.uv.fs_unlink(private)
        local proceed, cleanup_error = require("louiselm.workflow.rehearsal").after_cleanup(
          removed == true,
          remove_error,
          controller.status == "disposed"
        )
        if proceed == nil then
          if cleanup_error then
            refuse(cleanup_error)
          end
          return
        end
        if not proceed then
          return callback(false, cleanup_error)
        end
        local decoded, report = pcall(nvim.json.decode, result.stdout)
        if
          result.code ~= 0
          or not decoded
          or type(report) ~= "table"
          or report.status ~= "passed"
          or report.schema ~= "louiselm.run-rehearsal/1"
        then
          callback(
            false,
            "Paid Run requires a passing rehearsal on this boot, release and exact inputs; run louiselm-control run rehearse --selection FILE --plugin PLUGIN --json"
          )
        else
          callback(true)
        end
      end)
    end)
    if not started then
      local removed, remove_error = nvim.uv.fs_unlink(private)
      if not removed then
        return false, "Readiness input cleanup failed; paid dispatch refused: " .. tostring(remove_error)
      end
      return false, "Paid Run requires a passing rehearsal; installed control CLI is unavailable"
    end
    pending = job
    return true
  end
  -- Opening the draft does not activate authority. Start rechecks after review,
  -- so an intervening boot, release, source, plugin or tracker change fails closed.
  local start = controller.start
  function controller:start(callback)
    if pending or self.status ~= "new" then
      return false, "Run readiness check is pending, started or disposed"
    end
    return check(function(ok, message)
      if not ok then
        return callback(false, message, self.summary)
      end
      local started, start_error = start(self, callback)
      if not started then
        callback(false, start_error, self.summary)
      end
    end)
  end
  local checked, check_error = check(function(ok, message)
    if not ok then
      local disposed, dispose_error = release()
      return refuse(not disposed and dispose_error or message)
    end
    local buffer, view_error = require("louiselm.ui.bead_run").open(controller, phone_owner)
    if not buffer then
      local disposed, dispose_error = release()
      return refuse(not disposed and dispose_error or view_error or "Run could not start")
    end
  end)
  if not checked then
    local disposed, dispose_error = release()
    refuse(not disposed and dispose_error or check_error)
  end
end

local registration, revision = nvim.g.louiselm_bead_run_registration, nvim.g.louiselm_bead_run_revision
if registration == nil and revision == nil then
  local controller, err = require("louiselm.workflow.operator").new(selection)
  if not controller then
    return refuse(err or "Run selection refused")
  end
  return launch(controller)
end

local leave
local pending, err = require("louiselm.workflow.prepared").open_registered(
  registration,
  revision,
  function(controller, _, message)
    if leave then
      nvim.api.nvim_del_autocmd(leave)
      leave = nil
    end
    if not controller then
      return refuse(message or "Registered Run is unavailable")
    end
    if
      controller.options.prepared == nil
      or controller.options.prepared.selection_sha256 ~= nvim.fn.sha256(selection_bytes)
    then
      local disposed, dispose_error = controller:dispose()
      return refuse(not disposed and dispose_error or "Phone-published selection differs; review and publish it again")
    end
    local phone, phone_error = require("louiselm.phone_control").start({ phone_control = { enabled = true } })
    if not phone then
      local disposed, dispose_error = controller:dispose()
      return refuse(not disposed and dispose_error or phone_error or "Phone controller is unavailable")
    end
    launch(controller, phone)
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
