local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")

local T = MiniTest.new_set()
local controller_id = string.rep("1", 32)
local operation_id = string.rep("2", 32)

local function fixture(initial)
  local state = nvim.tbl_extend("force", {
    id = "session-1",
    agent = "mock",
    working_dir = nvim.uv.cwd(),
    status = "ready",
  }, initial or {})
  local owner = { sessions = {} }
  local session = { state = state, owner = owner }
  function session:inspect()
    return nvim.deepcopy(self.state)
  end
  function session:on(callback)
    self.observe = callback
    return function()
      self.observe = nil
    end
  end
  owner.sessions[state.id] = session
  local controller =
    Controller.new({ service = { nvim.fn.getcwd() .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  require("tests.fixtures.phone_control_history")(session, controller)
  assert(controller:attach(controller_id))
  local subject_id = assert(controller:track(session))
  local request = {
    v = 1,
    type = "read_session",
    controller_id = controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(state.working_dir)) }, agents = { "mock" }, groups = { "read" } },
    period = {},
    proof = {},
    request = {
      controller_id = controller_id,
      subject_id = subject_id,
      operation_id = operation_id,
      action = 1,
      payload = "00000000000000000014",
      expected_revision = 0,
    },
  }
  MiniTest.finally(function()
    controller:dispose()
  end)
  return controller, session, request
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

T["a shortened queue entry makes the live page incomplete"] = function()
  local controller, session, request = fixture()
  function session:queue_inspect()
    return {
      revision = 2,
      paused = true,
      uncertain = false,
      entries = { { id = string.rep("1", 32), display_text = string.rep("é", 1000) } },
    }
  end
  local page = read(controller, request)
  MiniTest.expect.equality(page.queue.entries[1].truncated, true)
  MiniTest.expect.equality(page.complete, false)
end

T["observation started during configuration does not reveal an unacknowledged value"] = function()
  local controller, session, request = fixture({
    status = "configuring",
    config_options = {
      {
        id = "model",
        name = "Model",
        type = "select",
        category = "model",
        current_value = "b",
        options = {
          { value = "b", name = "Model B" },
        },
      },
    },
  })
  local result = read(controller, request)
  MiniTest.expect.equality(result.settings, { model = "", effort = "", mode = "" })
  session.state.status = "ready"
  session.history.revision = session.history.revision + 1
  session.observe({ type = "config_options_changed", data = session.state.config_options })
  MiniTest.expect.equality(read(controller, request).settings.model, "Model B")
end

T["accepts a verified exact controller and current scoped Session"] = function()
  local controller, _, request = fixture()
  local result = read(controller, request)
  MiniTest.expect.equality(result.type, "session_page")
  MiniTest.expect.equality(result.subject_id, request.request.subject_id)
  MiniTest.expect.equality(result.operation_id, operation_id)
end

T["fails closed on verifier errors and independently checks scope and identity"] = function()
  local controller, session, request = fixture()
  request.request.operation_id = string.rep("f", 32)
  MiniTest.expect.equality(read(controller, request).code, "unauthenticated")
  request.request.operation_id = operation_id
  request.grant.agents = { "different" }
  MiniTest.expect.equality(read(controller, request).code, "out_of_scope")
  request.grant.agents = { "mock" }
  request.grant.groups = { "operate_sessions" }
  MiniTest.expect.equality(read(controller, request).code, "out_of_scope")
  request.grant.groups = { "read" }
  request.request.controller_id = string.rep("3", 32)
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
  request.request.controller_id = controller_id
  session.owner.sessions[session.state.id] = { state = session.state }
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
end

T["rejects non-verified output, subprocess failure, and project boundary escapes"] = function()
  local controller, session, request = fixture()
  request.request.operation_id = string.rep("e", 32)
  MiniTest.expect.equality(read(controller, request).code, "unavailable")
  request.request.operation_id = string.rep("d", 32)
  MiniTest.expect.equality(read(controller, request).code, "unavailable")
  request.request.operation_id = operation_id
  request.grant.projects = { assert(nvim.uv.fs_realpath(session.state.working_dir)) .. "-other" }
  MiniTest.expect.equality(read(controller, request).code, "out_of_scope")
  session.state.working_dir = "/path-that-cannot-exist/phone-control"
  MiniTest.expect.equality(read(controller, request).code, "unavailable")
end

T["refuses a late verifier result after Session disposal"] = function()
  local controller, session, request = fixture()
  local result
  controller:read(nvim.json.encode(request), function(value)
    result = value
  end)
  session.state.status = "disposed"
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  MiniTest.expect.equality(result.code, "stale_subject")
end

T["refuses a revision changed while verification is in flight"] = function()
  local controller, _, request = fixture()
  local projection = controller.subjects[request.request.subject_id].projection
  request.request.expected_revision = projection.revision
  local result
  controller:read(nvim.json.encode(request), function(value)
    result = value
  end)
  projection:record({ type = "state_changed", data = { status = "running" } })
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  MiniTest.expect.equality(result.code, "stale_subject")
end

T["fences an owner page after a revision change or Session removal"] = function()
  for _, change in ipairs({ "revision", "untrack" }) do
    local controller, session, request = fixture()
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
    if change == "revision" then
      controller.subjects[request.request.subject_id].projection:record({ type = "state_changed", data = {} })
    else
      controller:untrack(session)
    end
    reply({ subject = "owner", revision = 1, status = "ready", entries = {}, next_before = 0, complete = true })
    MiniTest.expect.equality(result.code, "stale_subject")
  end
end

T["cancels owner reads and suppresses late pages after reconnect"] = function()
  local controller, session, request = fixture()
  local reply, cancelled
  function session:read_history(_, callback)
    reply = callback
    return function()
      cancelled = true
    end
  end
  local called = false
  controller:read(nvim.json.encode(request), function()
    called = true
  end)
  assert(nvim.wait(5000, function()
    return reply ~= nil
  end))
  assert(controller:detach())
  assert(controller:attach(string.rep("3", 32)))
  reply({ subject = "owner", revision = 1, status = "ready", entries = {}, next_before = 0, complete = true })
  MiniTest.expect.equality(cancelled, true)
  MiniTest.expect.equality(called, false)
end

T["refuses a previously attached controller identity"] = function()
  local controller = fixture()
  controller:detach()
  local attached, err = controller:attach(controller_id)
  MiniTest.expect.equality(attached, false)
  MiniTest.expect.equality(err, "controller identity was already used")
end

T["disposal terminates an outstanding verifier and suppresses its callback"] = function()
  local controller, _, request = fixture()
  request.request.operation_id = string.rep("c", 32)
  local called = false
  controller:read(nvim.json.encode(request), function()
    called = true
  end)
  local pid = controller.process.pid
  controller:dispose()
  assert(nvim.wait(5000, function()
    return nvim.uv.kill(pid, 0) == nil
  end))
  nvim.wait(30)
  MiniTest.expect.equality(called, false)
end

T["reports verifier cancellation errors after invalidating authority"] = function()
  local controller = fixture()
  controller.process = {
    kill = function()
      error("synthetic kill failure")
    end,
  }
  local detached, err = controller:detach()
  MiniTest.expect.equality(detached, false)
  MiniTest.expect.equality(err, "could not stop phone-control verifier")
  MiniTest.expect.equality(controller.controller_id, nil)
end

T["returns an advancing cursor when escaped content exceeds the frame bound"] = function()
  local controller, _, request = fixture()
  local projection = controller.subjects[request.request.subject_id].projection
  for _ = 1, 20 do
    projection:record({
      type = "prompt_dispatched",
      data = { prompt = { { type = "text", text = string.rep("\1", 8192) } } },
    })
  end
  local page = read(controller, request)
  MiniTest.expect.equality(page.type, "session_page")
  MiniTest.expect.equality(#nvim.json.encode(page) + 1 <= 262144, true)
  MiniTest.expect.equality(#page.entries < 20, true)
  MiniTest.expect.equality(page.next_before, page.entries[1].seq)
  MiniTest.expect.equality(page.complete, true)
end

return T
