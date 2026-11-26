local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Journal = require("louiselm.phone_control.action_journal")
local Action = require("louiselm.phone_control.action")

local T = MiniTest.new_set()

local function exercise_phases(initialized)
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local directory, previous = nvim.fn.tempname(), nvim.env.LOUISELM_CAPTURE_STATE_DIR
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
    project = assert(nvim.uv.fs_realpath(nvim.fn.getcwd())),
    agent = "mock",
    recorded_at_ms = 1000,
  }
  local function run(message)
    local result
    journal:run(message, function(value)
      result = value
    end)
    assert(nvim.wait(5000, function()
      return result ~= nil
    end))
    return result
  end
  MiniTest.expect.equality(run({ v = 1, type = "inspect_phases", identity = identity }).code, "unavailable")
  local begun = run({ v = 2, type = "begin", generation = string.rep("09", 16), identity = identity })
  if initialized then
    MiniTest.expect.equality(begun.type, "begun")
  else
    MiniTest.expect.equality(begun, { v = 2, type = "error", code = "unavailable" })
  end
  for index, state in ipairs({ "accepted", "dispatched", "completed" }) do
    local report = {
      v = 1,
      type = "report",
      identity = identity,
      outcome = { [state] = { evidence = string.rep(tostring(index), 64) } },
      at_ms = 1000 + index,
    }
    local result = run(report)
    if initialized then
      MiniTest.expect.equality(result.type, "recorded")
    else
      MiniTest.expect.equality(result, { v = 1, type = "error", code = "unavailable" })
    end
  end
  local phases = run({ v = 1, type = "inspect_phases", identity = identity })
  if not initialized then
    MiniTest.expect.equality(phases, { v = 1, type = "error", code = "unavailable" })
    MiniTest.expect.equality(run({ v = 1, type = "inspect", identity = identity }), phases)
    MiniTest.expect.equality(nvim.uv.fs_stat(directory .. "/louiselm/workflow/phone-action-owner"), nil)
    return
  end
  MiniTest.expect.equality(phases.type, "action_phases")
  MiniTest.expect.equality(phases.accepted_evidence, string.rep("1", 64))
  MiniTest.expect.equality(phases.dispatched_evidence, string.rep("2", 64))
  MiniTest.expect.equality(phases.completed_evidence, string.rep("3", 64))
  local existing = run({ v = 1, type = "inspect", identity = identity })
  MiniTest.expect.equality(existing.outcome.completed.evidence, string.rep("3", 64))
end

T["an uninitialized owner refuses phase reports and inspection"] = function()
  exercise_phases(false)
end

T["inspection returns retained owner phases through the asynchronous helper"] = function()
  MiniTest.skip("restore after receiver-bound initialization: louiselm-qbr.12.4.3.2")
  exercise_phases(true)
end

local function exercise_detach(initialized)
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local directory, previous = nvim.fn.tempname(), nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local journal = Journal.new({ service = { nvim.fn.fnamemodify(binary, ":p") } })
  local permit, prompts = nil, 0
  local owner = { sessions = {} }
  local session = { owner = owner }
  owner.sessions.s = session
  function session:inspect()
    return { id = "s", status = "ready" }
  end
  function session:prompt()
    prompts = prompts + 1
    return "turn-1"
  end
  local action = Action.new({
    journal = journal,
    generation = string.rep("09", 16),
    session = session,
    projection = {
      page = function()
        return {}
      end,
    },
    current = function()
      return true
    end,
    authorize = function(done)
      permit = done
    end,
  })
  MiniTest.finally(function()
    action:dispose()
    journal:dispose()
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
    nvim.fn.delete(directory, "rf")
  end)
  require("tests.fixtures.phone_control_journal")(nvim.fn.fnamemodify(binary, ":p"), directory)
  local text = "private phone text"
  local identity = {
    operation_id = string.rep("11", 16),
    receiver_identity = string.rep("02", 32),
    controller_id = string.rep("03", 16),
    subject_id = string.rep("04", 16),
    action = 3,
    payload_hash = nvim.fn.sha256(string.char(1) .. text),
    grant_id = string.rep("06", 16),
    grant_revision = 1,
    period_id = string.rep("07", 16),
    expected_revision = 2,
    project = assert(nvim.uv.fs_realpath(nvim.fn.getcwd())),
    agent = "mock",
    recorded_at_ms = 1000,
  }
  local reply
  action:send(identity, text, function(value)
    if initialized then
      error("detached reply must not be delivered")
    end
    reply = value
  end)
  assert(nvim.wait(5000, function()
    return permit ~= nil or reply ~= nil
  end))
  if not initialized then
    local unavailable = { v = 1, type = "error", code = "unavailable" }
    MiniTest.expect.equality(reply, unavailable)
    MiniTest.expect.equality(permit, nil)
    local detached
    action:detach(function(value)
      detached = value
    end)
    assert(nvim.wait(5000, function()
      return detached ~= nil
    end))
    MiniTest.expect.equality(detached, unavailable)
    local duplicate
    action:send(identity, text, function(value)
      duplicate = value
    end)
    assert(nvim.wait(5000, function()
      return duplicate ~= nil
    end))
    MiniTest.expect.equality(duplicate, unavailable)
    MiniTest.expect.equality(prompts, 0)
    return
  end
  assert(permit ~= nil)
  local fenced
  action:detach(function(value)
    fenced = value
  end)
  assert(nvim.wait(5000, function()
    return fenced ~= nil
  end))
  MiniTest.expect.equality(fenced.state, "fenced_no_effect")
  local released = false
  assert(permit)({
    valid = function()
      return true
    end,
    release = function()
      released = true
    end,
  })
  local duplicate
  action:send(identity, text, function(value)
    duplicate = value
  end)
  assert(nvim.wait(5000, function()
    return duplicate ~= nil
  end))
  MiniTest.expect.equality(duplicate.type, "existing")
  MiniTest.expect.equality(duplicate.outcome.fenced_no_effect.evidence, fenced.evidence)
  MiniTest.expect.equality({ prompts, released }, { 0, true })
end

T["an uninitialized owner refuses Send and detach before authorization"] = function()
  exercise_detach(false)
end

T["a live owner fences a begun original before its late authorization"] = function()
  MiniTest.skip("restore after receiver-bound initialization: louiselm-qbr.12.4.3.2")
  exercise_detach(true)
end

return T
