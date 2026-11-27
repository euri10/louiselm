local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Process = require("louiselm.phone_control.process_identity")

local T = MiniTest.new_set()

T["the configured capture executable verifies the editor's retained process"] = function()
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local exited = false
  local child = nvim.system({ "cat" }, { stdin = true }, function()
    exited = true
  end)
  local identity = assert(Process.new(child.pid, assert(nvim.uv.fs_readlink("/proc/self/ns/pid")), { binary }))
  MiniTest.finally(function()
    identity:dispose()
    if not exited then
      child:kill(9)
    end
  end)
  local opened, cause, fast
  identity:open(function(ok, err)
    opened, cause, fast = ok, err, nvim.in_fast_event()
  end)
  assert(nvim.wait(10000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality({ opened, cause, fast, identity:alive() }, { true, nil, false, true })
  child:write(nil)
  assert(nvim.wait(5000, function()
    return exited
  end))
  MiniTest.expect.equality(identity:alive(), false)
end

return T
