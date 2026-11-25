local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Session = require("louiselm.session")
local Controller = require("louiselm.phone_control.controller")
local T = MiniTest.new_set()

T["discovers real registry Sessions without Attention and fences replacements"] = function()
  local root = nvim.fn.getcwd()
  local directory = nvim.fn.tempname()
  local definition = {
    provider = "test-service",
    command = nvim.v.progpath,
    env = { LOUISELM_MOCK_MODE = "static" },
    args = {
      "--headless",
      "--noplugin",
      "-u",
      root .. "/tests/mock/init.lua",
      "-c",
      "lua require('louiselm.dev.mock_agent').run()",
    },
  }
  local api = assert(Session.new({ mock = definition }, nil, { usage_directory = directory }))
  local replacement_api
  local controller = Controller.new({ service = { root .. "/tests/fixtures/phone_control_verify_stub.sh" } })
  local controller_id = string.rep("1", 32)
  MiniTest.finally(function()
    assert(controller:dispose())
    for _, owner in ipairs({ api, replacement_api }) do
      assert(owner:dispose())
      local finished = false
      owner:flush_recording(function()
        finished = true
      end)
      assert(nvim.wait(6000, function()
        return finished
      end))
    end
    nvim.fn.delete(directory, "rf")
    nvim.fn.delete(directory .. "-replacement", "rf")
  end)
  assert(controller:attach(controller_id))
  local first = assert(api:create_session("mock", { cwd = root, name = "same" }))
  local second = assert(api:create_session("mock", { cwd = root, name = "same" }))
  local first_id = assert(controller:track(first))
  local second_id = assert(controller:track(second))
  assert(nvim.wait(6000, function()
    return first:inspect().status == "ready" and second:inspect().status == "ready"
  end))
  local request = {
    v = 1,
    type = "list_sessions",
    controller_id = controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock" }, groups = { "read" } },
    period = {},
    proof = {},
    request = {
      controller_id = controller_id,
      subject_id = string.rep("0", 32),
      operation_id = string.rep("2", 32),
      action = 49,
      payload = string.rep("0", 32) .. "32",
      expected_revision = 0,
    },
  }
  local function read()
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
  MiniTest.expect.equality(page.type, "session_list")
  MiniTest.expect.equality(#page.sessions, 2)
  local ids = {}
  for _, row in ipairs(page.sessions) do
    ids[row.subject_id] = row.name
  end
  MiniTest.expect.equality(ids[first_id], "same")
  MiniTest.expect.equality(ids[second_id], "same")

  assert(first:dispose())
  controller:untrack(first)
  replacement_api = assert(Session.new({ mock = definition }, nil, { usage_directory = directory .. "-replacement" }))
  local replacement = assert(replacement_api:create_session("mock", { cwd = root, name = "same" }))
  local replacement_id = assert(controller:track(replacement))
  assert(nvim.wait(6000, function()
    return replacement:inspect().status == "ready"
  end))
  MiniTest.expect.equality(replacement:inspect().id, "session-1")
  MiniTest.expect.no_equality(replacement_id, first_id)
  request.type = "read_session"
  request.request.action = 1
  request.request.subject_id = first_id
  request.request.payload = "00000000000000000014"
  MiniTest.expect.equality(read().code, "stale_subject")
  controller:detach()
  assert(controller:attach(string.rep("3", 32)))
  MiniTest.expect.equality(read().code, "stale_subject")
end

return T
