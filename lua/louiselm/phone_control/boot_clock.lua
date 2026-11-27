---Suspend-aware native deadlines for phone admission.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the proc descriptor.
local nvim = vim
local ProcFile = require("louiselm.phone_control.proc_file")
local M = {}
local Clock = {}
Clock.__index = Clock
local MAX_INTEGER = 9007199254740991

local function namespace()
  local value = nvim.uv.fs_readlink("/proc/thread-self/ns/time")
  if type(value) == "string" and value:match("^time:%[%d+%]$") then
    return value
  end
end

local function centiseconds(value)
  local seconds, fraction = value:match("^(%d+)%.(%d%d)%s+%d+%.%d%d%s*$")
  seconds, fraction = tonumber(seconds), tonumber(fraction)
  if seconds == nil or fraction == nil or seconds > math.floor((MAX_INTEGER - fraction) / 100) then
    return nil
  end
  return seconds * 100 + fraction
end

-- vim.system cannot pass an existing descriptor as stdin. The child verifies
-- this exact handle and never reads it; the editor retains it for final checks.
local function verify(self, callback)
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
  nvim.list_extend(args, { "phone-control", "verify-clock" })
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
        self.error = err or "could not stop clock verifier"
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
    stdio = { self.fd, stdout },
  }, function(code, signal)
    if process ~= nil then
      process:close()
    end
    process = nil
    exited = code == 0 and signal == 0
    if not completed and (not exited or eof) then
      finish(exited and output == '{"v":1,"type":"clock_verified"}\n')
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
        finish(output == '{"v":1,"type":"clock_verified"}\n')
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

---Open and validate a Linux boot-time source asynchronously.
---@param callback fun(opened: boolean, error_message: string?) Called on the main loop unless disposed.
function Clock:open(callback)
  if self.state ~= "new" then
    nvim.schedule(function()
      if self.state ~= "disposed" then
        callback(false, "unavailable")
      end
    end)
    return
  end
  self.state = "opening"
  ProcFile.open("/proc/uptime", function(fd, err)
    if self.state == "disposed" then
      if fd ~= nil then
        local closed, cause = nvim.uv.fs_close(fd)
        if not closed then
          self.error = cause
        end
      end
      return
    end
    if fd == nil then
      self.state = "failed"
      callback(false, err or "unavailable")
      return
    end
    self.fd, self.state = fd, "validating"
    verify(self, function(valid)
      if self.state == "disposed" then
        return
      end
      self.state = valid and "open" or "failed"
      if valid and self:sample() == nil then
        self.state = "failed"
      end
      if self.state ~= "open" then
        local closed, cause = nvim.uv.fs_close(fd)
        self.fd = nil
        if not closed then
          self.error = cause
        end
      end
      callback(self.state == "open", self.state ~= "open" and "unavailable" or nil)
    end)
  end)
end

---Sample CLOCK_BOOTTIME through the pinned proc file without yielding.
---Linux truncates proc uptime to centiseconds. Namespace changes, backwards
---time and unavailable or malformed sources permanently invalidate this clock.
---@return {namespace: string, centiseconds: integer}? sample
function Clock:sample()
  if self.state ~= "open" or self.fd == nil then
    return nil
  end
  local before = namespace()
  local value = nvim.uv.fs_read(self.fd, 129, 0)
  local after = namespace()
  local now = type(value) == "string" and #value <= 128 and centiseconds(value) or nil
  if
    before == nil
    or before ~= after
    or (self.namespace ~= nil and self.namespace ~= before)
    or now == nil
    or (self.last ~= nil and now < self.last)
  then
    self.state = "failed"
    return nil
  end
  self.namespace, self.last = before, now
  return { namespace = before, centiseconds = now }
end

---Sample only a clock created by this module; do not invoke caller callbacks.
---@param clock table
---@return {namespace: string, centiseconds: integer}? sample
function M.sample(clock)
  if type(clock) ~= "table" or getmetatable(clock) ~= Clock then
    return nil
  end
  return Clock.sample(clock)
end

---Check the owner's final interval against a receiver-supplied remaining bound.
---Charge the missing proc centisecond; a boundary or inconsistent clock fails closed.
---@param start {namespace: string, centiseconds: integer}
---@param now {namespace: string, centiseconds: integer}
---@param remaining_ms integer At most five seconds; measured no earlier than start.
---@return boolean valid
function M.within(start, now, remaining_ms)
  if
    type(start) ~= "table"
    or type(now) ~= "table"
    or type(start.namespace) ~= "string"
    or start.namespace:match("^time:%[%d+%]$") == nil
    or start.namespace ~= now.namespace
    or type(start.centiseconds) ~= "number"
    or type(now.centiseconds) ~= "number"
    or start.centiseconds < 0
    or start.centiseconds > MAX_INTEGER
    or start.centiseconds % 1 ~= 0
    or now.centiseconds < start.centiseconds
    or now.centiseconds > MAX_INTEGER
    or now.centiseconds % 1 ~= 0
    or type(remaining_ms) ~= "number"
    or remaining_ms <= 0
    or remaining_ms > 5000
    or remaining_ms % 1 ~= 0
  then
    return false
  end
  return (now.centiseconds - start.centiseconds + 1) * 10 < remaining_ms
end

---Release the proc descriptor and suppress a late open callback.
---@return boolean closed
---@return string? error_message
function Clock:dispose()
  local fd = self.fd
  self.fd, self.state = nil, "disposed"
  if self.verifier ~= nil then
    self.verifier.cancel()
  end
  if fd == nil then
    return self.error == nil, self.error
  end
  local closed, err = nvim.uv.fs_close(fd)
  return closed ~= nil and self.error == nil, err or self.error
end

---Create an unopened clock without I/O.
---@param service string[] Configured capture-service argv prefix.
---@return table clock
function M.new(service)
  return setmetatable({ state = "new", service = type(service) == "table" and nvim.deepcopy(service) or nil }, Clock)
end

return M
