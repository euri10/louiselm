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
local disposed, dispose_error = controller:dispose()
if not disposed then
  return refuse(dispose_error or "Run selection could not be released")
end
-- louiselm-rrfgt blocks safe rehearsal of failures before the first receipt.
-- No attestation can pass yet; keep this entrypoint before Run authorization.
return refuse(
  "Paid Run requires a passing rehearsal on this boot and release; blocked by louiselm-rrfgt. "
    .. "Run louiselm-control run rehearse --json"
)
