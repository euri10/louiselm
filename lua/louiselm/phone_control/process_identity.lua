---Pin one Linux process for the phone admission liveness check.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the event loop and proc descriptors.
local nvim = vim
local ProcFile = require("louiselm.phone_control.proc_file")
local Verifier = require("louiselm.phone_control.descriptor_verifier")
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
    if fd == nil then
      if self.state ~= "disposed" then
        self.state = "failed"
        callback(false, err or "unavailable")
      end
      return
    end
    if self.state == "disposed" then
      local closed, close_error = nvim.uv.fs_close(fd)
      if not closed then
        self.error = close_error
      end
      return
    end
    self.fd, self.state = fd, "validating"
    local function finish(valid)
      if self.state == "disposed" then
        return
      end
      self.state = valid and "open" or "failed"
      if valid and not self:alive() then
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
    end
    ProcFile.open("/proc/self/status", function(status_fd)
      if self.state == "disposed" then
        if status_fd ~= nil then
          local closed, cause = nvim.uv.fs_close(status_fd)
          if not closed then
            self.error = cause
          end
        end
        return
      end
      if status_fd == nil then
        finish(false)
        return
      end
      self.status_fd = status_fd
      Verifier.start(
        self,
        {
          "phone-control",
          "verify-process",
          "--pid",
          tostring(self.pid),
          "--editor-pid",
          tostring(nvim.uv.os_getpid()),
          "--namespace",
          self.namespace,
        },
        '{"v":1,"type":"process_verified"}\n',
        function(valid)
          if self.state == "disposed" then
            return
          end
          local closed, cause = nvim.uv.fs_close(status_fd)
          self.status_fd = nil
          if not closed then
            self.error = cause
          end
          finish(valid and closed ~= nil)
        end
      )
    end)
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
  local fd, status_fd = self.fd, self.status_fd
  self.fd, self.status_fd, self.state = nil, nil, "disposed"
  if self.verifier ~= nil then
    self.verifier.cancel()
  end
  if status_fd ~= nil then
    local closed, err = nvim.uv.fs_close(status_fd)
    if not closed then
      self.error = err
    end
  end
  if fd == nil then
    return self.error == nil, self.error
  end
  local closed, err = nvim.uv.fs_close(fd)
  return closed ~= nil and self.error == nil, err or self.error
end

---Check only an identity created by this module; do not invoke caller callbacks.
---@param identity table
---@return boolean alive
function M.alive(identity)
  return type(identity) == "table" and getmetatable(identity) == Identity and Identity.alive(identity)
end

---Create an unopened process identity. This performs no I/O.
---@param pid integer PID in this process's proc namespace.
---@param namespace string PID namespace reported by the peer on its original connection.
---@param service string[] Configured capture-service argv prefix.
---@return table? identity
---@return string? error_message
function M.new(pid, namespace, service)
  if
    type(pid) ~= "number"
    or pid < 1
    or pid > 2147483647
    or pid % 1 ~= 0
    or type(namespace) ~= "string"
    or namespace:match("^pid:%[%d+%]$") == nil
  then
    return nil, "unavailable"
  end
  return setmetatable({
    pid = pid,
    namespace = namespace,
    service = type(service) == "table" and nvim.deepcopy(service) or nil,
    state = "new",
  }, Identity),
    nil
end

return M
