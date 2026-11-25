---Pin one Linux process for the phone admission liveness check.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the event loop and proc descriptors.
local nvim = vim
local M = {}
local Identity = {}
Identity.__index = Identity

local function read_file(path, limit, callback)
  nvim.uv.fs_open(path, "r", 0, function(open_error, fd)
    if open_error ~= nil or fd == nil then
      callback(nil)
      return
    end
    nvim.uv.fs_read(fd, limit + 1, 0, function(read_error, value)
      nvim.uv.fs_close(fd, function(close_error)
        if read_error ~= nil or close_error ~= nil or type(value) ~= "string" or #value > limit then
          callback(nil)
        else
          callback(value)
        end
      end)
    end)
  end)
end

local function proc_mount(mountinfo, mount_id)
  for line in mountinfo:gmatch("[^\n]+") do
    local id, root, path = line:match("^(%d+) %d+ %S+ (%S+) (%S+) ")
    local filesystem = line:match(" %- (%S+) ")
    if id == mount_id and root == "/" and path == "/proc" and filesystem == "proc" then
      return true
    end
  end
  return false
end

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
  local function finish(fd)
    nvim.schedule(function()
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
      callback(fd ~= nil, fd == nil and "unavailable" or nil)
    end)
  end
  nvim.uv.fs_open("/proc/" .. self.pid .. "/stat", "r", 0, function(open_error, fd)
    if open_error ~= nil or fd == nil then
      finish(nil)
      return
    end
    local function reject()
      nvim.uv.fs_close(fd, function(close_error)
        if close_error ~= nil then
          self.error = close_error
        end
        finish(nil)
      end)
    end
    read_file("/proc/thread-self/fdinfo/" .. fd, 4096, function(info)
      local mount_id = info and info:match("\nmnt_id:%s*(%d+)")
      if mount_id == nil then
        reject()
        return
      end
      read_file("/proc/thread-self/mountinfo", 1048576, function(mountinfo)
        if mountinfo == nil or not proc_mount(mountinfo, mount_id) then
          reject()
        else
          finish(fd)
        end
      end)
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
