local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Controller = require("louiselm.phone_control.controller")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local T = MiniTest.new_set()

T["reads the exact Session's confirmed options after a real ACP acknowledgement"] = function()
  local root, directory = nvim.fn.getcwd(), nvim.fn.tempname()
  local options = {
    {
      id = "model",
      name = "Model",
      category = "model",
      type = "select",
      currentValue = "a",
      options = {
        { value = "a", name = "Model A" },
        { value = "b", name = "Model B" },
      },
    },
    {
      id = "effort",
      name = "Effort",
      category = "thought_level",
      type = "select",
      currentValue = "high",
      options = {
        { value = "high", name = "High" },
      },
    },
    {
      id = "access",
      name = "Access",
      category = "mode",
      type = "select",
      currentValue = "full",
      options = {
        { value = "full", name = "Full access" },
      },
    },
    {
      id = "private",
      name = "Private",
      category = "other",
      type = "select",
      currentValue = "secret",
      options = {
        { value = "secret", name = "PRIVATE CONFIG" },
      },
    },
  }
  local function definition(advertised)
    return {
      provider = "test-service",
      command = nvim.v.progpath,
      args = {
        "--headless",
        "--noplugin",
        "-u",
        root .. "/tests/mock/init.lua",
        "-c",
        "lua require('louiselm.dev.mock_agent').run(" .. nvim.inspect({ config_options = advertised }) .. ")",
      },
    }
  end
  local api = assert(Session.new({ mock = definition(options), missing = definition({}) }, nil, {
    usage_directory = directory,
  }))
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
  assert(controller:attach(string.rep("1", 32)))
  local first = assert(api:create_session("mock", { cwd = root }))
  local first_id = assert(controller:track(first))
  local other = assert(api:create_session("missing", { cwd = root }))
  local other_id = assert(controller:track(other))
  assert(nvim.wait(6000, function()
    return first:inspect().status == "ready" and other:inspect().status == "ready"
  end))
  local request = {
    v = 1,
    type = "read_session",
    controller_id = string.rep("1", 32),
    grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock", "missing" }, groups = { "read" } },
    proof = {},
    period = {},
    request = {
      controller_id = string.rep("1", 32),
      subject_id = first_id,
      operation_id = string.rep("2", 32),
      action = 1,
      payload = "00000000000000000014",
      expected_revision = 0,
    },
  }
  local function read()
    local reply
    controller:read(nvim.json.encode(request), function(value)
      reply = value
    end)
    assert(nvim.wait(6000, function()
      return reply ~= nil
    end))
    return reply
  end
  local page = read()
  MiniTest.expect.equality(page.settings, { model = "Model A", effort = "High", mode = "Full access" })
  MiniTest.expect.equality(nvim.json.encode(page):find("PRIVATE CONFIG", 1, true), nil)
  local acknowledged = false
  assert(first:set_config_option("model", "b", function(_, err)
    assert(err == nil, err)
    acknowledged = true
  end))
  MiniTest.expect.equality(controller.subjects[first_id].projection.settings.model, "Model A")
  -- This notification-before-response ordering is hypothetical; the mock response is still pending.
  local premature = first:inspect().config_options
  premature[1].current_value = "b"
  first.emitter:emit({ type = "config_options_changed", session_id = first:inspect().id, data = premature })
  MiniTest.expect.equality(controller.subjects[first_id].projection.settings.model, "")
  assert(nvim.wait(6000, function()
    return acknowledged
  end))
  request.request.expected_revision = page.revision
  MiniTest.expect.equality(read().code, "stale_subject")
  request.request.expected_revision = 0
  local updated = read()
  MiniTest.expect.equality(updated.settings.model, "Model B")
  first.emitter:emit({
    type = "config_options_changed",
    session_id = first:inspect().id,
    data = first:inspect().config_options,
  })
  request.request.expected_revision = updated.revision
  MiniTest.expect.equality(read().code, "stale_subject")
  request.request.expected_revision = 0
  request.request.subject_id = other_id
  MiniTest.expect.equality(read().settings, { model = "", effort = "", mode = "" })
  request.grant.groups = { "operate_sessions" }
  MiniTest.expect.equality(read().code, "out_of_scope")
end

return T
