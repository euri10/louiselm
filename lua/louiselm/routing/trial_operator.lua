---Trusted, explicit operator entrypoint for qualification-run.py.
local Trial = require("louiselm.routing.trial")
local PrivateFile = require("louiselm.private_file")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}

---Run the private prepared selection named by the invoking operator process.
---Refusals exit nonzero; result artifacts never contain selected prompts/output.
---The caller must invoke this only in its fresh private headless Neovim process.
---@return nil
function M.start()
  local path, output = nvim.env.LOUISELM_QUALIFICATION_SELECTION, nvim.env.LOUISELM_QUALIFICATION_OUTPUT
  local metadata = path and nvim.uv.fs_lstat(path)
  if
    not metadata
    or metadata.type ~= "file"
    or metadata.uid ~= nvim.uv.os_getuid()
    or metadata.mode % 512 ~= 384
    or metadata.size > 1000000
    or type(output) ~= "string"
  then
    nvim.cmd("cquit 1")
    return
  end
  local read, lines = pcall(nvim.fn.readfile, path)
  if not read then
    nvim.cmd("cquit 1")
    return
  end
  local decoded, selection = pcall(nvim.json.decode, table.concat(lines, "\n"))
  if not decoded then
    nvim.cmd("cquit 1")
    return
  end
  local controller, err = Trial.new(selection)
  if not controller then
    nvim.cmd("cquit 1")
    return
  end
  local leave = nvim.api.nvim_create_autocmd("VimLeavePre", {
    once = true,
    callback = function()
      local ok, dispose_error = controller:dispose()
      if not ok then
        nvim.v.errmsg = dispose_error or "trial disposal failed"
      end
    end,
  })
  local started, start_error = controller:start(function(result, failure)
    local disposed, dispose_error = controller:dispose()
    nvim.api.nvim_del_autocmd(leave)
    if failure then
      result.observations.error = failure
    end
    if not disposed then
      result.observations.cleanup_error = "local disposal failed"
    end
    local encoded, report = pcall(nvim.json.encode, result.report)
    local encoded_observations, observations = pcall(nvim.json.encode, result.observations)
    if not encoded or not encoded_observations then
      nvim.cmd("cquit 1")
      return
    end
    local written, write_error =
      PrivateFile.write(nvim.uv, output .. "/report.json", report .. "\n", "trial report", "replace")
    local observed, observation_error =
      PrivateFile.write(nvim.uv, output .. "/observations.json", observations .. "\n", "trial observations", "replace")
    if not written or not observed or not disposed or failure then
      nvim.cmd("cquit 1")
    else
      nvim.cmd("qa!")
    end
  end)
  if not started then
    local disposed, dispose_error = controller:dispose()
    nvim.api.nvim_del_autocmd(leave)
    nvim.cmd("cquit 1")
  end
end

return M
