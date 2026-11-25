local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local BootClock = require("louiselm.phone_control.boot_clock")
local ProcFile = require("louiselm.phone_control.proc_file")

local T = MiniTest.new_set()

T["the native boot clock reads a validated Linux proc source"] = function()
  local clock = BootClock.new()
  MiniTest.finally(function()
    clock:dispose()
  end)
  local opened
  clock:open(function(ok)
    opened = ok
  end)
  assert(nvim.wait(5000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality(opened, true)
  local first = assert(clock:sample())
  local second = assert(clock:sample())
  MiniTest.expect.equality(first.namespace, second.namespace)
  MiniTest.expect.equality(second.centiseconds >= first.centiseconds, true)
  MiniTest.expect.equality(BootClock.within(first, second, 5000), true)
  clock:dispose()
  MiniTest.expect.equality(clock:sample(), nil)
end

T["the final deadline counts all elapsed boot time conservatively"] = function()
  local start = { namespace = "time:[1]", centiseconds = 100 }
  local function sample(centiseconds, namespace)
    return { namespace = namespace or "time:[1]", centiseconds = centiseconds }
  end
  MiniTest.expect.equality(BootClock.within(start, sample(100), 11), true)
  MiniTest.expect.equality(BootClock.within(start, sample(100), 10), false)
  MiniTest.expect.equality(BootClock.within(start, sample(598), 5000), true)
  MiniTest.expect.equality(BootClock.within(start, sample(599), 5000), false)
  MiniTest.expect.equality(BootClock.within(start, sample(600), 5000), false)
  MiniTest.expect.equality(BootClock.within(start, sample(99), 5000), false)
  MiniTest.expect.equality(BootClock.within(start, sample(101, "time:[2]"), 5000), false)
  MiniTest.expect.equality(BootClock.within(start, sample(100), 5001), false)
  MiniTest.expect.equality(BootClock.within(start, sample(100), 0), false)
  MiniTest.expect.equality(BootClock.within(start, sample(100.5), 5000), false)
end

T["a descriptor outside the root proc mount is refused"] = function()
  local finished, result, cause, fast = false, nil, nil, nil
  ProcFile.open("/dev/null", function(fd, err)
    finished, result, cause, fast = true, fd, err, nvim.in_fast_event()
  end)
  assert(nvim.wait(5000, function()
    return finished
  end))
  MiniTest.expect.equality({ result, cause, fast }, { nil, "unavailable", false })
end

return T
