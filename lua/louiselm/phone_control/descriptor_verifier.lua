---Run the configured capture helper against an already-open descriptor.

---@diagnostic disable-next-line: undefined-global -- Neovim owns process and descriptor lifetimes.
local nvim = vim
local M = {}

-- vim.system cannot pass existing descriptors. The child verifies these exact
-- handles; the editor retains its process or clock handle for final checks.
---@param self table Owner of the descriptors and verifier lifecycle.
---@param command string[] Helper arguments following the configured service prefix.
---@param response string Exact successful response.
---@param callback fun(valid: boolean) Scheduled completion.
function M.start(self, command, response, callback)
  local stdout = nvim.uv.new_pipe(false)
  local timer = nvim.uv.new_timer()
  if stdout == nil or timer == nil or self.service == nil or #self.service == 0 then
    if stdout then
      stdout:close()
    end
    if timer then
      timer:close()
    end
    nvim.schedule(function()
      callback(false)
    end)
    return
  end
  local args = {}
  for index = 2, #self.service do
    args[#args + 1] = self.service[index]
  end
  nvim.list_extend(args, command)
  local env = {}
  for _, key in ipairs({
    "PATH",
    "HOME",
    "LANG",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
  }) do
    if nvim.env[key] ~= nil then
      env[#env + 1] = key .. "=" .. nvim.env[key]
    end
  end
  ---@alias ClockVerifierKill fun(self: table, signal: integer): integer?, string?
  local process ---@type {kill: ClockVerifierKill, close: fun(self: table)}?
  local completed, exited, eof, output = false, false, false, ""
  local function finish(valid)
    if completed then
      return
    end
    completed = true
    self.verifier = nil
    timer:stop()
    timer:close()
    stdout:read_stop()
    stdout:close()
    if process ~= nil then
      local killed, err = process:kill(9)
      if killed == nil and (type(err) ~= "string" or not err:match("ESRCH")) then
        self.error = err or "could not stop descriptor verifier"
      end
    end
    nvim.schedule(function()
      callback(valid)
    end)
  end
  self.verifier = {
    cancel = function()
      finish(false)
    end,
  }
  local cwd = nvim.uv.cwd()
  if cwd == nil then
    finish(false)
    return
  end
  local spawned, handle = pcall(nvim.uv.spawn, self.service[1], {
    args = args,
    cwd = cwd,
    env = env,
    stdio = { self.fd, stdout, self.status_fd },
  }, function(code, signal)
    if process ~= nil then
      process:close()
    end
    process = nil
    exited = code == 0 and signal == 0
    if not completed and (not exited or eof) then
      finish(exited and output == response)
    end
  end)
  if not spawned or handle == nil then
    finish(false)
    return
  end
  process = handle
  local reading = stdout:read_start(function(err, data)
    if err ~= nil then
      finish(false)
    elseif data == nil then
      eof = true
      if exited then
        finish(output == response)
      end
    else
      output = output .. data
      if #output > 128 then
        finish(false)
      end
    end
  end)
  local started = timer:start(5000, 0, function()
    finish(false)
  end)
  if reading == nil or started == nil then
    finish(false)
  end
end

return M
