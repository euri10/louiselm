---Open a bounded Linux proc source without trusting a replacement mount.

---@diagnostic disable-next-line: undefined-global -- Neovim owns the filesystem boundary.
local nvim = vim
local M = {}

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

---Open a proc file and verify the actual descriptor's root proc mount.
---@param path string Trusted proc path.
---@param callback fun(fd: integer?, error_message: string?) Called on the main loop; owns a returned descriptor.
function M.open(path, callback)
  local function finish(fd, cause)
    nvim.schedule(function()
      callback(fd, cause)
    end)
  end
  nvim.uv.fs_open(path, "r", 0, function(open_error, fd)
    if open_error ~= nil or fd == nil then
      finish(nil, "unavailable")
      return
    end
    local function reject()
      nvim.uv.fs_close(fd, function(close_error)
        finish(nil, close_error or "unavailable")
      end)
    end
    -- libuv filesystem requests may run on different worker threads. /proc/self
    -- names Neovim's leader, so mountinfo describes the owner thread's mount
    -- namespace rather than whichever worker happened to open mountinfo.
    read_file("/proc/self/fdinfo/" .. fd, 4096, function(info)
      local mount_id = info and info:match("\nmnt_id:%s*(%d+)")
      if mount_id == nil then
        reject()
        return
      end
      read_file("/proc/self/mountinfo", 1048576, function(mountinfo)
        if mountinfo == nil or not proc_mount(mountinfo, mount_id) then
          reject()
        else
          finish(fd)
        end
      end)
    end)
  end)
end

return M
