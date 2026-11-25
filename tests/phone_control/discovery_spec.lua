local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")

local T = MiniTest.new_set()
local controller_id = string.rep("1", 32)
local zero = string.rep("0", 32)

local function fixture()
  local root = assert(nvim.uv.fs_realpath(nvim.uv.cwd()))
  local owner = { sessions = {}, order = {} }
  function owner:list_sessions()
    return nvim.deepcopy(self.order)
  end
  function owner:get_session(id)
    return self.sessions[id]
  end
  local controller =
    Controller.new({ service = { nvim.fn.getcwd() .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  assert(controller:attach(controller_id))
  local function add(id, agent, cwd, name)
    local session =
      { owner = owner, state = { id = id, name = name, agent = agent, working_dir = cwd, status = "ready" } }
    function session:inspect()
      return nvim.deepcopy(self.state)
    end
    owner.sessions[id] = session
    owner.order[#owner.order + 1] = id
    return session, assert(controller:track(session))
  end
  MiniTest.finally(function()
    assert(controller:dispose())
  end)
  local request = {
    v = 1,
    type = "list_sessions",
    controller_id = controller_id,
    grant = { projects = { root }, agents = { "mock" }, groups = { "read" } },
    period = {},
    proof = {},
    request = {
      controller_id = controller_id,
      subject_id = zero,
      operation_id = string.rep("2", 32),
      action = 49,
      payload = zero .. "32",
      expected_revision = 0,
    },
  }
  return controller, owner, add, request, root
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

T["lists only scoped live Sessions with exact incarnation identities"] = function()
  local controller, owner, add, request, root = fixture()
  local first, first_id = add("session-1", "mock", root, "same")
  local _, second_id = add("session-2", "mock", root, "same")
  add("session-3", "other", root, "hidden agent")
  add("session-4", "mock", "/tmp", "hidden project")
  local page = read(controller, request)
  MiniTest.expect.equality(page.type, "session_list")
  MiniTest.expect.equality(page.controller_id, controller_id)
  MiniTest.expect.equality(#page.sessions, 2)
  local by_id = {}
  for _, row in ipairs(page.sessions) do
    by_id[row.subject_id] = row
  end
  MiniTest.expect.equality(by_id[first_id].name, "same")
  MiniTest.expect.equality(by_id[second_id].name, "same")
  MiniTest.expect.equality(by_id[first_id].agent, "mock")
  MiniTest.expect.equality(by_id[first_id].project, root)
  MiniTest.expect.equality(by_id[first_id].status, "ready")
  owner.sessions[first.state.id] = nil
  page = read(controller, request)
  MiniTest.expect.equality(#page.sessions, 1)
  MiniTest.expect.equality(page.sessions[1].subject_id, second_id)
  request.grant.groups = { "operate_sessions" }
  MiniTest.expect.equality(read(controller, request).code, "out_of_scope")
end

T["pages by subject identity and refuses a replaced controller"] = function()
  local controller, _, add, request, root = fixture()
  local _, first_id = add("session-1", "mock", root, string.rep("x", 1000))
  local _, second_id = add("session-2", "mock", root, "same")
  request.request.payload = zero .. "01"
  local first = read(controller, request)
  MiniTest.expect.equality(#first.sessions, 1)
  MiniTest.expect.equality(first.next_after, first.sessions[1].subject_id)
  request.request.payload = first.next_after .. "01"
  local second = read(controller, request)
  MiniTest.expect.equality(#second.sessions, 1)
  MiniTest.expect.no_equality(second.sessions[1].subject_id, first.sessions[1].subject_id)
  MiniTest.expect.equality(second.next_after, "")
  local all = { [first.sessions[1].subject_id] = true, [second.sessions[1].subject_id] = true }
  MiniTest.expect.equality(all[first_id] and all[second_id], true)
  local long = first.sessions[1].subject_id == first_id and first.sessions[1] or second.sessions[1]
  MiniTest.expect.equality(#long.name, 512)
  MiniTest.expect.equality(long.truncated, true)
  controller:detach()
  assert(controller:attach(string.rep("3", 32)))
  MiniTest.expect.equality(read(controller, request).code, "stale_subject")
end

T["rejects a Session-read payload in a discovery request"] = function()
  local controller, _, _, request = fixture()
  request.request.payload = "00000000000000000014"
  MiniTest.expect.equality(read(controller, request).code, "malformed")
end

return T
