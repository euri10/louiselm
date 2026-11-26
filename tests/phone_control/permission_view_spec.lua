local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Controller = require("louiselm.phone_control.controller")
local Projection = require("louiselm.phone_control.projection")
local Permission = require("louiselm.permission")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local T = MiniTest.new_set()

T["a late phone read follows the desktop permission responder"] = function()
  local root = nvim.fn.getcwd()
  local directory = nvim.fn.tempname()
  local api = assert(Session.new({
    mock = {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        root .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run()",
      },
      env = { LOUISELM_MOCK_MODE = "permission" },
    },
  }, nil, { usage_directory = directory }))
  local controller = Controller.new({ service = { root .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  MiniTest.finally(function()
    assert(controller:dispose())
    assert(api:dispose())
    local flushed = false
    api:flush_recording(function()
      flushed = true
    end)
    assert(nvim.wait(6000, function()
      return flushed
    end))
    nvim.fn.delete(directory, "rf")
  end)
  local permission
  local session = assert(api:create_session("mock", {
    cwd = root,
    on_event = function(event)
      if event.type == "permission_requested" then
        permission = event
      end
    end,
  }))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  assert(session:prompt("fixture request", function() end))
  assert(nvim.wait(6000, function()
    return permission ~= nil
  end))
  local controller_id = string.rep("1", 32)
  assert(controller:attach(controller_id))
  local subject_id = assert(controller:track(session))
  local request = {
    v = 1,
    type = "read_session",
    controller_id = controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock" }, groups = { "read" } },
    proof = {},
    period = {},
    request = {
      controller_id = controller_id,
      subject_id = subject_id,
      operation_id = string.rep("2", 32),
      action = 1,
      payload = "00000000000000000032",
      expected_revision = 0,
    },
  }
  local function read(revision)
    request.request.expected_revision = revision or 0
    local result
    controller:read(nvim.json.encode(request), function(value)
      result = value
    end)
    assert(nvim.wait(6000, function()
      return result ~= nil
    end))
    return result
  end
  local page = read()
  MiniTest.expect.equality(page.type, "session_page")
  MiniTest.expect.equality(
    page.current_permission,
    { title = "Edit mock file", options = { "Allow once", "Reject" }, incomplete = false }
  )
  local encoded = nvim.json.encode(page)
  MiniTest.expect.equality(encoded:find("mock.txt", 1, true), nil)
  MiniTest.expect.equality(encoded:find("allow-once", 1, true), nil)
  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  MiniTest.expect.equality(read(page.revision).code, "stale_subject")
  MiniTest.expect.equality(read().current_permission, nil)
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  permission = nil
  assert(session:prompt("another request", function() end))
  assert(nvim.wait(6000, function()
    return permission ~= nil
  end))
  local waiting = read()
  MiniTest.expect.equality(waiting.current_permission.title, "Edit mock file")
  assert(session:cancel())
  MiniTest.expect.equality(read(waiting.revision).code, "stale_subject")
  MiniTest.expect.equality(read().current_permission, nil)
  local automatic, completed = false, false
  local policy = assert(Permission.policy("auto-approve-scoped", { paths = { "mock.txt" } }))
  local auto_session = assert(api:create_session("mock", {
    cwd = root,
    permission_policy = policy,
    on_event = function(event)
      if event.type == "permission_requested" then
        automatic = true
      end
    end,
  }))
  assert(nvim.wait(6000, function()
    return auto_session:inspect().status == "ready"
  end))
  assert(auto_session:prompt("automatic", function(_, err)
    assert(err == nil, err)
    completed = true
  end))
  assert(nvim.wait(6000, function()
    return completed
  end))
  MiniTest.expect.equality(automatic, false)
end

T["permission display bounds and marks missing fields"] = function()
  local choices = {}
  for index = 1, 51 do
    choices[index] = { name = "choice " .. index, optionId = "private" }
  end
  local view = Projection.permission({
    request_id = 1,
    operation = { kind = "unknown" },
    policy_decision = "ask",
    toolCall = { title = string.rep("é", 5000), rawInput = { env = "secret" } },
    options = choices,
  })
  MiniTest.expect.equality(#view.title, 8192)
  MiniTest.expect.equality(#view.options, 50)
  MiniTest.expect.equality(view.incomplete, true)
  MiniTest.expect.equality(nvim.json.encode(view):find("secret", 1, true), nil)
  MiniTest.expect.equality(
    Projection.permission({
      request_id = 1,
      operation = { kind = "unknown" },
      policy_decision = "ask",
      options = { {} },
    }),
    { title = "", options = { "" }, incomplete = true }
  )
  MiniTest.expect.equality(
    Projection.permission({
      request_id = 1,
      operation = { kind = "unknown" },
      policy_decision = "ask",
      toolCall = { title = "title" },
      options = {},
    }),
    { title = "title", options = {}, incomplete = true }
  )
end

return T
