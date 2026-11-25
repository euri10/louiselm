local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Journal = require("louiselm.phone_control.action_journal")

local T = MiniTest.new_set()

local function exercise_journal(initialized)
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local root = nvim.fn.getcwd()
  local directory = nvim.fn.tempname()
  local previous = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local journal = Journal.new({ service = { nvim.fn.fnamemodify(binary, ":p") } })
  MiniTest.finally(function()
    journal:dispose()
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
    nvim.fn.delete(directory, "rf")
  end)
  require("tests.fixtures.phone_control_journal")(nvim.fn.fnamemodify(binary, ":p"), directory)
  local identity = {
    operation_id = string.rep("01", 16),
    receiver_identity = string.rep("02", 32),
    controller_id = string.rep("03", 16),
    subject_id = string.rep("04", 16),
    action = 3,
    payload_hash = string.rep("05", 32),
    grant_id = string.rep("06", 16),
    grant_revision = 1,
    period_id = string.rep("07", 16),
    expected_revision = 2,
    project = assert(nvim.uv.fs_realpath(root)),
    agent = "mock",
    recorded_at_ms = 1000,
  }
  local function call(message)
    local result
    journal:run(message, function(value)
      result = value
    end)
    assert(nvim.wait(5000, function()
      return result ~= nil
    end))
    return result
  end
  MiniTest.expect.equality(call({ v = 1, type = "inspect", identity = identity }).code, "unavailable")
  local fenced = call({ v = 1, type = "fence", identity = identity, at_ms = 2000 })
  if not initialized then
    local unavailable = { v = 1, type = "error", code = "unavailable" }
    MiniTest.expect.equality(fenced, unavailable)
    MiniTest.expect.equality(call({ v = 1, type = "begin", identity = identity }), unavailable)
    MiniTest.expect.equality(call({ v = 1, type = "inspect", identity = identity }), unavailable)
    MiniTest.expect.equality(call({ v = 1, type = "inspect_phases", identity = identity }), unavailable)
    MiniTest.expect.equality(
      call({
        v = 1,
        type = "report",
        identity = identity,
        outcome = { accepted = { evidence = string.rep("1", 64) } },
        at_ms = 2001,
      }),
      unavailable
    )
    identity.payload_hash = string.rep("08", 32)
    MiniTest.expect.equality(call({ v = 1, type = "begin", identity = identity }), unavailable)
    MiniTest.expect.equality(nvim.uv.fs_stat(directory .. "/louiselm/workflow/phone-action-owner"), nil)
    MiniTest.expect.equality(nvim.uv.fs_stat(directory .. "/louiselm/capture/tls/phone-action-owner.json.lock"), nil)
    return
  end
  MiniTest.expect.equality(fenced.type, "fenced")
  local inspected = call({ v = 1, type = "inspect", identity = identity })
  MiniTest.expect.equality(inspected.outcome.fenced_no_effect.evidence, fenced.evidence)
  local late = call({ v = 1, type = "begin", identity = identity })
  MiniTest.expect.equality(late.type, "existing")
  MiniTest.expect.equality(late.outcome.fenced_no_effect.evidence, fenced.evidence)
  identity.payload_hash = string.rep("08", 32)
  MiniTest.expect.equality(call({ v = 1, type = "inspect", identity = identity }).code, "conflict")
  MiniTest.expect.equality(call({ v = 1, type = "begin", identity = identity }).code, "conflict")
end

T["an uninitialized native journal refuses every operation without creating evidence"] = function()
  exercise_journal(false)
end

T["the asynchronous native journal retains a fence and rejects a late begin"] = function()
  MiniTest.skip("restore after receiver-bound initialization: louiselm-qbr.12.4.3.2")
  exercise_journal(true)
end

T["unavailable helpers fail closed and disposal suppresses late callbacks"] = function()
  local bad = Journal.new({ service = { "/path-that-cannot-exist/louiselm-capture" } })
  local result
  bad:run({ v = 1, type = "begin", identity = {} }, function(value)
    result = value
  end)
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  MiniTest.expect.equality(result.code, "unavailable")
  assert(bad:dispose())

  local journal =
    Journal.new({ service = { "/bin/sh", nvim.fn.getcwd() .. "/tests/fixtures/phone_action_journal_stub.sh" } })
  local begun
  journal:run({ v = 1, type = "begin" }, function(value)
    begun = value
  end)
  assert(nvim.wait(5000, function()
    return begun ~= nil
  end))
  MiniTest.expect.equality(begun.type, "begun")
  local malformed
  journal:run({ v = 1, type = "fence" }, function(value)
    malformed = value
  end)
  assert(nvim.wait(5000, function()
    return malformed ~= nil
  end))
  MiniTest.expect.equality(malformed.code, "unavailable")
  local called = false
  journal:run({ v = 1, type = "report" }, function()
    called = true
  end)
  assert(journal:dispose())
  nvim.wait(100)
  MiniTest.expect.equality(called, false)
  assert(journal:dispose())
end

return T
