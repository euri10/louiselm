local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Clock = require("louiselm.phone_control.boot_clock")

local T = MiniTest.new_set()

T["the real capture executable verifies the editor's retained clock"] = function()
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local clock = Clock.new({ nvim.fn.fnamemodify(binary, ":p") })
  MiniTest.finally(function()
    clock:dispose()
  end)
  local opened, cause, fast
  clock:open(function(ok, err)
    opened, cause, fast = ok, err, nvim.in_fast_event()
  end)
  assert(nvim.wait(10000, function()
    return opened ~= nil
  end))
  MiniTest.expect.equality({ opened, cause, fast }, { true, nil, false })
  local first, second = assert(clock:sample()), assert(clock:sample())
  MiniTest.expect.equality(first.namespace, second.namespace)
  MiniTest.expect.equality(second.centiseconds >= first.centiseconds, true)
  MiniTest.expect.equality(Clock.within(first, second, 5000), true)
  assert(clock:dispose())
  MiniTest.expect.equality(clock:sample(), nil)
end

return T
