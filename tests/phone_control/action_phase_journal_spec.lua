local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Journal = require("louiselm.phone_control.action_journal")
local Action = require("louiselm.phone_control.action")

local T = MiniTest.new_set()

T["inspection returns retained owner phases through the asynchronous helper"] = function()
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
  MiniTest.expect.equality(run({ v = 1, type = "begin", identity = identity }).type, "begun")
  for index, state in ipairs({ "accepted", "dispatched", "completed" }) do
    local report = {
      v = 1,
      type = "report",
      identity = identity,
      outcome = { [state] = { evidence = string.rep(tostring(index), 64) } },
      at_ms = 1000 + index,
    }
    MiniTest.expect.equality(run(report).type, "recorded")
  end
  local phases = run({ v = 1, type = "inspect_phases", identity = identity })
  MiniTest.expect.equality(phases.type, "action_phases")
  MiniTest.expect.equality(phases.accepted_evidence, string.rep("1", 64))
  MiniTest.expect.equality(phases.dispatched_evidence, string.rep("2", 64))
  MiniTest.expect.equality(phases.completed_evidence, string.rep("3", 64))
  local existing = run({ v = 1, type = "inspect", identity = identity })
  MiniTest.expect.equality(existing.outcome.completed.evidence, string.rep("3", 64))
end

T["a live owner fences a begun original before its late authorization"] = function()
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
  action:send(identity, text, function()
    error("detached reply must not be delivered")
  end)
  assert(nvim.wait(5000, function()
    return permit ~= nil
  end))
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

return T
