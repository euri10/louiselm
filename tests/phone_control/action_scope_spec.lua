local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")

local T = MiniTest.new_set()

T["the live owner proves only distinct exact Session incarnations independent"] = function()
  local controller_id = string.rep("1", 32)
  local controller = Controller.new({ service = { "unused" } })
  assert(controller:attach(controller_id))
  local owner = { sessions = {} }
  local function session(id)
    local item = { owner = owner }
    function item:inspect()
      return { id = id, status = "ready", working_dir = nvim.fn.getcwd(), agent = "mock" }
    end
    owner.sessions[id] = item
    require("tests.fixtures.phone_control_history")(item, controller)
    return item
  end
  local first, second = session("first"), session("second")
  local old, target = assert(controller:track(first)), assert(controller:track(second))
  MiniTest.finally(function()
    controller:dispose()
  end)
  local request = {
    v = 1,
    type = "action_independence",
    controller_id = controller_id,
    subject_id = target,
    operation_id = string.rep("2", 32),
    unknowns = {
      { operation_id = string.rep("3", 32), controller_id = controller_id, subject_id = old },
      { operation_id = string.rep("4", 32), controller_id = controller_id, subject_id = target },
      { operation_id = string.rep("5", 32), controller_id = string.rep("6", 32), subject_id = old },
    },
  }
  local function read(line)
    local result
    controller:read(line, function(value)
      result = value
    end)
    assert(nvim.wait(3000, function()
      return result ~= nil
    end))
    return result
  end
  local result = read(nvim.json.encode(request))
  MiniTest.expect.equality(result.type, "action_independence")
  MiniTest.expect.equality(result.independent, { string.rep("3", 32) })
  assert(controller:detach())
  controller_id = string.rep("7", 32)
  assert(controller:attach(controller_id))
  target = assert(controller:track(second))
  request.controller_id, request.subject_id = controller_id, target
  MiniTest.expect.equality(read(nvim.json.encode(request)).independent, { string.rep("3", 32) })
  owner.sessions.first = nil
  MiniTest.expect.equality(read(nvim.json.encode(request)).independent, {})
  local duplicate = nvim.json.encode(request):gsub('"type":', '"type":"action_independence","type":')
  MiniTest.expect.equality(read(duplicate).code, "malformed")
  request.unknowns = {}
  for index = 1, 51 do
    request.unknowns[index] =
      { operation_id = string.format("%032x", index), controller_id = controller_id, subject_id = old }
  end
  MiniTest.expect.equality(read(nvim.json.encode(request)).code, "malformed")
  request.unknowns = {}
  owner.sessions.second = {}
  MiniTest.expect.equality(read(nvim.json.encode(request)).code, "stale_subject")
end

T["a local scope lookup identifies only the exact live Session incarnation"] = function()
  local controller_id = string.rep("1", 32)
  local state = { id = "s", status = "ready", working_dir = nvim.fn.getcwd(), agent = "mock" }
  local owner = { sessions = {} }
  local session = { owner = owner }
  function session:inspect()
    return nvim.deepcopy(state)
  end
  owner.sessions.s = session
  local controller = Controller.new({ service = { "unused" } })
  require("tests.fixtures.phone_control_history")(session, controller)
  assert(controller:attach(controller_id))
  local subject = assert(controller:track(session))
  MiniTest.finally(function()
    controller:dispose()
  end)
  local request = {
    v = 1,
    type = "action_scope",
    controller_id = controller_id,
    subject_id = subject,
    operation_id = string.rep("2", 32),
  }
  local function read()
    local result
    controller:read(nvim.json.encode(request), function(value)
      result = value
    end)
    assert(nvim.wait(5000, function()
      return result ~= nil
    end))
    return result
  end
  local scope = read()
  MiniTest.expect.equality(scope.type, "action_scope")
  MiniTest.expect.equality(scope.project, assert(nvim.uv.fs_realpath(state.working_dir)))
  MiniTest.expect.equality(scope.agent, "mock")
  MiniTest.expect.equality(scope.subject_id, subject)
  request.controller_id = string.rep("3", 32)
  MiniTest.expect.equality(read().code, "stale_subject")
  request.controller_id = controller_id
  request.role = "operator"
  MiniTest.expect.equality(read().code, "malformed")
  request.role = nil
  local duplicate
  local raw = nvim.json.encode(request):gsub('"type":', '"type":"action_scope","type":')
  controller:read(raw, function(value)
    duplicate = value
  end)
  assert(nvim.wait(5000, function()
    return duplicate ~= nil
  end))
  MiniTest.expect.equality(duplicate.code, "malformed")
  owner.sessions.s = {}
  MiniTest.expect.equality(read().code, "stale_subject")
end

return T
