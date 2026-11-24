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
local module = nvim.api.nvim_get_runtime_file("lua/louiselm/workflow/operator.lua", false)[1]
if not module then
  return refuse("Trusted LouiseLM plugin is unavailable")
end
local plugin = nvim.fs.dirname(nvim.fs.dirname(nvim.fs.dirname(nvim.fs.dirname(module))))
local pending
local function check(callback)
  -- The controller owns the selection parsed above, not later edits of its file.
  local private =
    string.format("%s/rehearsal-check-%d-%.0f.json", selection.snapshot_parent, nvim.uv.os_getpid(), nvim.uv.hrtime())
  local written, write_error = require("louiselm.private_file").write(
    nvim.uv,
    private,
    table.concat(lines, "\n"),
    "Run readiness selection",
    "publish"
  )
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
      if not removed then
        return refuse("Readiness input cleanup failed; paid dispatch refused: " .. tostring(remove_error))
      end
      if controller.status == "disposed" then
        return
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
    local disposed, dispose_error = controller:dispose()
    return refuse(not disposed and dispose_error or message)
  end
  local buffer, view_error = require("louiselm.ui.bead_run").open(controller)
  if not buffer then
    return refuse(view_error or "Run could not start")
  end
end)
if not checked then
  local disposed, dispose_error = controller:dispose()
  refuse(not disposed and dispose_error or check_error)
end
