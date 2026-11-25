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
    self.fd = fd
    self.state = fd and "open" or "failed"
    if fd ~= nil and self:sample() == nil then
      local closed, cause = nvim.uv.fs_close(fd)
      self.fd, self.state = nil, "failed"
      if not closed then
        self.error = cause
      end
    end
    callback(self.state == "open", self.state ~= "open" and (err or "unavailable") or nil)
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
  if fd == nil then
    return self.error == nil, self.error
  end
  local closed, err = nvim.uv.fs_close(fd)
  return closed ~= nil, err
end

---Create an unopened clock without I/O.
---@return table clock
function M.new()
  return setmetatable({ state = "new" }, Clock)
end

return M
