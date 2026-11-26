local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")
local T = MiniTest.new_set()
local controller_id = string.rep("1", 32)
local zero = string.rep("0", 32)

T["reads only configured and granted Agents without any live Sessions"] = function()
  local controller = Controller.new({
    service = { nvim.fn.getcwd() .. "/tests/fixtures/phone_control_verify_stub.sh" },
    agents = { allowed = { command = "private-command" }, removed = {}, unapproved = {} },
  })
  MiniTest.finally(function()
    assert(controller:dispose())
  end)
  assert(controller:attach(controller_id))
  local message = {
    v = 1,
    type = "list_selection",
    controller_id = controller_id,
    grant = { projects = {}, agents = { "allowed", "not-configured" }, groups = { "read", "operate_sessions" } },
    period = {},
    proof = {},
    request = {
      controller_id = controller_id,
      subject_id = zero,
      operation_id = string.rep("2", 32),
      action = 50,
      payload = zero .. "32",
      expected_revision = 0,
    },
  }
  local function read(owner)
    local result
    (owner or controller):read(nvim.json.encode(message), function(value)
      result = value
    end)
    assert(nvim.wait(5000, function()
      return result ~= nil
    end))
    return result
  end
  local result = read()
  MiniTest.expect.equality(result.type, "selection_agents")
  MiniTest.expect.equality(result.controller_id, controller_id)
  MiniTest.expect.equality(result.agents, { "allowed" })
  MiniTest.expect.equality(nvim.json.encode(result):find("private%-command"), nil)
  local empty = Controller.new({ service = { nvim.fn.getcwd() .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  MiniTest.finally(function()
    assert(empty:dispose())
  end)
  assert(empty:attach(controller_id))
  MiniTest.expect.equality(read(empty).agents, {})
  message.grant.groups = { "operate_sessions" }
  MiniTest.expect.equality(read().code, "out_of_scope")
  message.grant.groups = { "read" }
  controller:detach()
  assert(controller:attach(string.rep("3", 32)))
  MiniTest.expect.equality(read().code, "stale_subject")
end

return T
