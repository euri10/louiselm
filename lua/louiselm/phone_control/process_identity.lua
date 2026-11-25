---Pin one Linux process for the phone admission liveness check.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the event loop and proc descriptors.
local nvim = vim
local ProcFile = require("louiselm.phone_control.proc_file")
local M = {}
local Identity = {}
Identity.__index = Identity

---Open and validate the proc descriptor asynchronously.
---The caller must bind it to a fresh response on the original connection after opening;
---the PID or this descriptor alone does not authenticate the peer.
---@param callback fun(opened: boolean, error_message: string?) Called on the main loop unless disposed.
function Identity:open(callback)
  if self.state ~= "new" then
    nvim.schedule(function()
      if self.state ~= "disposed" then
        callback(false, "unavailable")
      end
    end)
    return
  end
  self.state = "opening"
  ProcFile.open("/proc/" .. self.pid .. "/stat", function(fd, err)
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
    callback(fd ~= nil, fd == nil and (err or "unavailable") or nil)
  end)
end

---Check the pinned process without yielding. A stopped process remains alive.
---Linux proc descriptors never retarget a reused numeric PID. Read only the bounded
---kernel-backed proc file; failures, dead states and unknown states fail closed.
---@return boolean alive
function Identity:alive()
  if self.state ~= "open" or self.fd == nil then
    return false
  end
  local value = nvim.uv.fs_read(self.fd, 4097, 0)
  if type(value) ~= "string" or #value > 4096 or value:sub(-1) ~= "\n" then
    return false
  end
  local pid, state = value:match("^(%d+) %(.+%) ([A-Za-z]) ")
  return pid == tostring(self.pid) and state ~= nil and ("RSDTtIWPK"):find(state, 1, true) ~= nil
end

---Release the pinned descriptor and suppress a late open callback.
---@return boolean closed
---@return string? error_message
function Identity:dispose()
  local fd = self.fd
  self.fd, self.state = nil, "disposed"
  if fd == nil then
    return self.error == nil, self.error
  end
  local closed, err = nvim.uv.fs_close(fd)
  return closed ~= nil, err
end

---Create an unopened process identity. This performs no I/O.
---@param pid integer PID in this process's proc namespace.
---@return table? identity
---@return string? error_message
function M.new(pid)
  if type(pid) ~= "number" or pid < 1 or pid > 2147483647 or pid % 1 ~= 0 then
    return nil, "unavailable"
  end
  return setmetatable({ pid = pid, state = "new" }, Identity), nil
end

return M
