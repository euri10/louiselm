local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local RunOwner = require("louiselm.phone_control.run_owner")

local T = MiniTest.new_set()
local RUN = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
local REGISTRATION = string.rep("1", 32)
local CONTROLLER = string.rep("2", 32)
local SUBJECT = string.rep("3", 32)

local function fixture()
  local owner = { sessions = {} }
  local session = { owner = owner, state = { id = "local", agent = "codex", status = "ready" } }
  function session:inspect()
    return nvim.deepcopy(self.state)
  end
  owner.sessions["local"] = session
  local run = { id = RUN, workers = { session }, status = "active" }
  session.owner_run = run
  local executor = {
    run = run,
    options = {
      envelope = { run_id = RUN },
      agent_id = "codex",
      prepared = { registration_id = REGISTRATION, revision = 2, selection_sha256 = string.rep("4", 64) },
      bead_ids = { "a", "b" },
    },
    status = "active",
    phone_admitted = true,
    current_progress = { phase = "working", index = 1, total = 2, bead_id = "a" },
  }
  local function subject(value)
    if value == session then
      return { controller_id = CONTROLLER, subject_id = SUBJECT }
    end
  end
  return RunOwner.new(), executor, session, subject
end

T["only projects an explicitly tracked exact owner and its current owned Session"] = function()
  local runs, executor, session, subject = fixture()
  MiniTest.expect.equality(runs:snapshot(RUN, CONTROLLER, subject), nil)
  assert(runs:track(executor))
  local snapshot = assert(runs:snapshot(RUN, CONTROLLER, subject))
  assert(type(snapshot.owner_id) == "string" and #snapshot.owner_id == 32)
  snapshot.owner_id = nil
  MiniTest.expect.equality(snapshot, {
    run_id = RUN,
    registration_id = REGISTRATION,
    registration_revision = 2,
    admitted = true,
    status = "active",
    phase = "working",
    bead_index = 1,
    bead_total = 2,
    bead_id = "a",
    sessions = { { subject_id = SUBJECT, agent = "codex", status = "ready" } },
    sessions_truncated = false,
  })
  session.owner.sessions["local"] = { state = session.state }
  MiniTest.expect.equality(assert(runs:snapshot(RUN, CONTROLLER, subject)).sessions, {})
  session.owner.sessions["local"] = session
  session.owner_run = { id = RUN }
  MiniTest.expect.equality(assert(runs:snapshot(RUN, CONTROLLER, subject)).sessions, {})
end

T["does not resolve a replacement, changed registration, or disposed owner"] = function()
  local runs, executor, _, subject = fixture()
  assert(runs:track(executor))
  local replacement = nvim.deepcopy(executor)
  MiniTest.expect.equality(runs:track(replacement), false)
  runs:untrack(replacement)
  MiniTest.expect.no_equality(runs:snapshot(RUN, CONTROLLER, subject), nil)
  executor.options.prepared.revision = 3
  MiniTest.expect.equality(runs:snapshot(RUN, CONTROLLER, subject), nil)
  executor.options.prepared.revision = 2
  executor.status = "disposed"
  MiniTest.expect.equality(runs:snapshot(RUN, CONTROLLER, subject), nil)
  runs:untrack(executor)
  MiniTest.expect.equality(runs:snapshot(RUN, CONTROLLER, subject), nil)
end

T["rejects unregistered owners and omits identities from another connection"] = function()
  local runs, executor, _, subject = fixture()
  local prepared = executor.options.prepared
  executor.options.prepared = nil
  MiniTest.expect.equality(runs:track(executor), false)
  executor.options.prepared = prepared
  assert(runs:track(executor))
  MiniTest.expect.equality(assert(runs:snapshot(RUN, string.rep("5", 32), subject)).sessions, {})
  executor.current_progress = nil
  executor.status = "new"
  executor.phone_admitted = false
  local waiting = assert(runs:snapshot(RUN, CONTROLLER, subject))
  MiniTest.expect.equality({ waiting.status, waiting.admitted, waiting.phase }, { "new", false, "none" })
end

T["a replacement Run owner receives a different incarnation on the same connection"] = function()
  local runs, executor, _, subject = fixture()
  assert(runs:track(executor))
  local original = assert(runs:snapshot(RUN, CONTROLLER, subject)).owner_id
  assert(type(original) == "string" and #original == 32)
  runs:untrack(executor)
  local replacement = nvim.deepcopy(executor)
  assert(runs:track(replacement))
  local current = assert(runs:snapshot(RUN, CONTROLLER, subject)).owner_id
  MiniTest.expect.no_equality(current, original)
end

return T
