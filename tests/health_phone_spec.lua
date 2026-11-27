local MiniTest = require("mini.test")
local Health = require("louiselm.health")
local Config = require("louiselm.config")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

local function fixture()
  local directory = nvim.fn.tempname() .. " helper space"
  assert(nvim.fn.mkdir(directory, "p") == 1)
  local helper = directory .. "/capture helper"
  local marker = directory .. "/called"
  assert(nvim.fn.writefile({
    "#!/bin/sh",
    "printf called > " .. nvim.fn.shellescape(marker),
    '[ "$#" = 5 ] && [ "$1" = "prefix value" ] && [ "$3" = phone-control ] && [ "$4" = verify-clock ] && [ "$5" = --help ] || exit 7',
    'case "$2" in old) exit 1 ;; interrupted) kill -TERM $$ ;; slow) sleep 1 ;; hung) trap "" TERM; exec sleep 30 ;; esac',
    "exit 0",
  }, helper) == 0)
  assert(nvim.uv.fs_chmod(helper, 448))
  local previous_health, previous_notify = nvim.health, nvim.notify
  local calls = { ok = {}, info = {}, warn = {}, error = {}, notifications = {} }
  nvim.health = { start = function() end }
  for _, level in ipairs({ "ok", "info", "warn", "error" }) do
    nvim.health[level] = function(message)
      calls[level][#calls[level] + 1] = message
    end
  end
  rawset(nvim, "notify", function(message, level)
    assert(not nvim.in_fast_event(), "notification must be scheduled")
    calls.notifications[#calls.notifications + 1] = { message = message, level = level }
  end)
  MiniTest.finally(function()
    Health.reset()
    nvim.health = previous_health
    rawset(nvim, "notify", previous_notify)
    nvim.fn.delete(directory, "rf")
  end)
  return helper, marker, calls
end

local function configure(helper, mode, enabled)
  local config = { capture = { enabled = false, service = { helper, "prefix value", mode } } }
  if enabled ~= nil then
    config.phone_control = { enabled = enabled }
  end
  assert(Health.configure(config, Config.schema))
end

local function notification(calls)
  assert(
    nvim.wait(3000, function()
      return #calls.notifications > 0
    end),
    "phone helper result was not reported"
  )
  return calls.notifications[1]
end

T["enabled phone control checks the configured argv asynchronously without desktop capture"] = function()
  local helper, marker, calls = fixture()
  configure(helper, "current", true)
  Health.check()
  MiniTest.expect.equality(table.concat(calls.info, "\n"):find("result will be notified", 1, true) ~= nil, true)
  local result = notification(calls)
  MiniTest.expect.equality(result.level, nvim.log.levels.INFO)
  MiniTest.expect.equality(result.message:find("phone-control verify-clock --help", 1, true) ~= nil, true)
  MiniTest.expect.equality(result.message:find("exited successfully", 1, true) ~= nil, true)
  MiniTest.expect.equality(result.message:find("Send readiness", 1, true) ~= nil, true)
  MiniTest.expect.equality(nvim.fn.filereadable(marker), 1)
end

T["old or invalid helper reports a manual update and command check"] = function()
  local helper, _, calls = fixture()
  configure(helper, "old", true)
  Health.check()
  local result = notification(calls)
  MiniTest.expect.equality(result.level, nvim.log.levels.WARN)
  MiniTest.expect.equality(result.message:find("may be older", 1, true) ~= nil, true)
  MiniTest.expect.equality(result.message:find("capture.service", 1, true) ~= nil, true)
  MiniTest.expect.equality(result.message:find("capture-service/README.md", 1, true) ~= nil, true)
end

T["missing helper warns immediately and disabled phone control does not run it"] = function()
  local helper, marker, calls = fixture()
  configure(helper .. ".missing", "current", true)
  Health.check()
  MiniTest.expect.equality(table.concat(calls.warn, "\n"):find("not executable", 1, true) ~= nil, true)
  MiniTest.expect.equality(table.concat(calls.warn, "\n"):find("capture-service/README.md", 1, true) ~= nil, true)
  configure(helper, "current", false)
  calls.info, calls.warn = {}, {}
  Health.check()
  nvim.wait(100, function()
    return false
  end)
  MiniTest.expect.equality(nvim.fn.filereadable(marker), 0)
  MiniTest.expect.equality(table.concat(calls.info, "\n"):find("Phone Send", 1, true), nil)
  MiniTest.expect.equality(calls.notifications, {})
end

T["unset phone control retains its enabled default"] = function()
  local helper, _, calls = fixture()
  configure(helper, "current")
  Health.check()
  MiniTest.expect.equality(notification(calls).level, nvim.log.levels.INFO)
end

T["interrupted probes are inconclusive and reset suppresses late results"] = function()
  local helper, _, calls = fixture()
  configure(helper, "interrupted", true)
  Health.check()
  local result = notification(calls)
  MiniTest.expect.equality(result.level, nvim.log.levels.WARN)
  MiniTest.expect.equality(result.message:find("could not determine", 1, true) ~= nil, true)
  calls.notifications = {}
  configure(helper, "slow", true)
  Health.check()
  Health.reset()
  nvim.wait(1200, function()
    return #calls.notifications > 0
  end)
  MiniTest.expect.equality(calls.notifications, {})
end

T["a hung helper is stopped and reported as inconclusive"] = function()
  local helper, _, calls = fixture()
  configure(helper, "hung", true)
  Health.check()
  assert(
    nvim.wait(7000, function()
      return #calls.notifications > 0
    end),
    "timed-out helper was not reported"
  )
  local result = calls.notifications[1]
  MiniTest.expect.equality(result.level, nvim.log.levels.WARN)
  MiniTest.expect.equality(result.message:find("could not determine", 1, true) ~= nil, true)
end

return T
