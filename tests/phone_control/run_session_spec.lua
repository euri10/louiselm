local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")

local T = MiniTest.new_set()
local RUN = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
local CONNECTION = string.rep("1", 32)
local OPERATION = string.rep("2", 32)

local function fixture()
  local owner = { sessions = {} }
  local session = {
    owner = owner,
    state = { id = "local", agent = "codex", working_dir = nvim.uv.cwd(), status = "ready" },
  }
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
      prepared = { registration_id = string.rep("3", 32), revision = 1, selection_sha256 = string.rep("4", 64) },
      bead_ids = { "b-1" },
    },
    status = "active",
    phone_admitted = true,
  }
  local controller =
    Controller.new({ service = { nvim.fn.getcwd() .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  require("tests.fixtures.phone_control_history")(session, controller)
  assert(controller:attach(CONNECTION))
  local subject = assert(controller:track(session))
  assert(controller:track_run(executor))
  local snapshot = assert(controller.runs:snapshot(RUN, CONNECTION, function()
    return { controller_id = CONNECTION, subject_id = subject }
  end))
  local function payload(run_id, owner_id)
    return "01" .. run_id:gsub("-", "") .. owner_id .. "0000000000000001" .. "00000000000000000014"
  end
  local request = {
    v = 1,
    type = "read_run_session",
    controller_id = CONNECTION,
    grant = { projects = { "/different-original-project" }, agents = { "codex" }, groups = { "read" } },
    period = {},
    proof = {},
    request = {
      action = 57,
      controller_id = CONNECTION,
      subject_id = subject,
      operation_id = OPERATION,
      expected_revision = 0,
      payload = payload(RUN, snapshot.owner_id),
    },
  }
  MiniTest.finally(function()
    controller:dispose()
  end)
  return controller, executor, session, request, payload, snapshot.owner_id
end

local function read(controller, request)
  local result
  controller:read(nvim.json.encode(request), function(value)
    result = value
  end)
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  return result
end

T["reads only a current exact Run-owned Session using owner provenance"] = function()
  local controller, _, _, request = fixture()
  local result = read(controller, request)
  MiniTest.expect.equality(result.type, "session_page")
  MiniTest.expect.equality(result.subject_id, request.request.subject_id)
  request.grant.groups = { "control_runs" }
  MiniTest.expect.equality(read(controller, request).code, "out_of_scope")
  request.grant.groups = { "read" }
  request.grant.agents = { "other" }
  MiniTest.expect.equality(read(controller, request).code, "out_of_scope")
end

T["refuses old owners, foreign Runs and unadmitted controllers"] = function()
  local controller, executor, session, request, payload, old_id = fixture()
  request.request.payload = payload(RUN, string.rep("5", 32))
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
  request.request.payload = payload("ffffffff-ffff-4fff-8fff-ffffffffffff", old_id)
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
  request.request.payload = payload(RUN, old_id)
  executor.phone_admitted = false
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
  executor.phone_admitted = true
  controller:untrack_run(executor)
  local replacement = nvim.deepcopy(executor)
  replacement.run.workers = { session }
  session.owner_run = replacement.run
  assert(controller:track_run(replacement))
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
end

T["rechecks Run ownership after an asynchronous Session history read"] = function()
  local controller, executor, session, request = fixture()
  local reply
  function session:read_history(_, callback)
    reply = callback
    return function() end
  end
  local result
  controller:read(nvim.json.encode(request), function(value)
    result = value
  end)
  assert(nvim.wait(5000, function()
    return reply ~= nil
  end))
  controller:untrack_run(executor)
  reply({ subject = "owner", revision = 1, status = "ready", entries = {}, next_before = 0, complete = true })
  MiniTest.expect.equality(result.code, "stale_subject")
end

return T
