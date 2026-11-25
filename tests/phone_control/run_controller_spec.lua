local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")

local T = MiniTest.new_set()
local RUN = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
local CONNECTION = string.rep("1", 32)

local function fixture()
  local session_owner = { sessions = {} }
  local session = { owner = session_owner, state = { id = "local", agent = "codex", status = "ready" } }
  function session:inspect()
    return nvim.deepcopy(self.state)
  end
  session_owner.sessions["local"] = session
  local run = { id = RUN, workers = { session }, status = "active" }
  session.owner_run = run
  local executor = {
    run = run,
    options = {
      envelope = { run_id = RUN },
      agent_id = "codex",
      prepared = { registration_id = string.rep("2", 32), revision = 1, selection_sha256 = string.rep("3", 64) },
      bead_ids = { "b-1" },
    },
    status = "active",
    phone_admitted = true,
  }
  local controller = Controller.new({ service = { "unused" } })
  require("tests.fixtures.phone_control_history")(session, controller)
  assert(controller:attach(CONNECTION))
  local subject_id = assert(controller:track(session))
  MiniTest.finally(function()
    controller:dispose()
  end)
  return controller, executor, session, subject_id
end

local function probe(controller, message)
  local result
  controller:read(nvim.json.encode(message), function(value)
    result = value
  end)
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  return result
end

T["probes only an explicitly registered Run and its current Session subjects"] = function()
  local controller, executor, _, subject_id = fixture()
  local request = { v = 1, type = "run_probe", controller_id = CONNECTION, run_id = RUN }
  MiniTest.expect.equality(probe(controller, request).state, "absent")
  assert(controller:track_run(executor))
  local found = probe(controller, request)
  MiniTest.expect.equality(found.type, "run_probe")
  MiniTest.expect.equality(found.state, "found")
  MiniTest.expect.equality(found.controller_id, CONNECTION)
  MiniTest.expect.equality(found.run.sessions[1].subject_id, subject_id)
  controller:untrack_run(executor)
  MiniTest.expect.equality(probe(controller, request).state, "absent")
end

T["old connection and Session identities never resolve after reconnect"] = function()
  local controller, executor, _, subject_id = fixture()
  assert(controller:track_run(executor))
  local original = probe(controller, { v = 1, type = "run_probe", controller_id = CONNECTION, run_id = RUN })
  assert(controller:detach())
  local new_connection = string.rep("4", 32)
  assert(controller:attach(new_connection))
  local request = { v = 1, type = "run_probe", controller_id = CONNECTION, run_id = RUN }
  MiniTest.expect.equality(probe(controller, request).code, "stale_subject")
  request.controller_id = new_connection
  local found = probe(controller, request)
  MiniTest.expect.no_equality(found.run.sessions[1].subject_id, subject_id)
  MiniTest.expect.no_equality(found.run.owner_id, original.run.owner_id)
  executor.status = "disposed"
  MiniTest.expect.equality(probe(controller, request).code, "stale_subject")
end

T["rejects malformed or extended probe requests"] = function()
  local controller = fixture()
  local request = { v = 1, type = "run_probe", controller_id = CONNECTION, run_id = RUN, extra = true }
  MiniTest.expect.equality(probe(controller, request).code, "malformed")
  request.extra = nil
  request.run_id = RUN:upper()
  MiniTest.expect.equality(probe(controller, request).code, "malformed")
end

return T
