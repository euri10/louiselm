local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")

local T = MiniTest.new_set()

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
