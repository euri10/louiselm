local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")
local T = MiniTest.new_set()

T["a Send waits for current authority after durable owner reservation"] = function()
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  local root, directory = nvim.fn.getcwd(), nvim.fn.tempname()
  local previous = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local state = { id = "s", status = "ready", agent = "mock", working_dir = root }
  local owner = { sessions = {} }
  local session, prompts = { owner = owner }, 0
  function session:inspect()
    return nvim.deepcopy(state)
  end
  function session:prompt(text)
    prompts = prompts + 1
    MiniTest.expect.equality(text, "phone text")
    return "turn-1"
  end
  owner.sessions.s = session
  local controller =
    Controller.new({ service = { "/bin/sh", root .. "/tests/fixtures/phone_control_action_stub.sh", binary } })
  assert(controller:attach(string.rep("01", 16)))
  local subject = assert(controller:track(session))
  MiniTest.finally(function()
    controller:dispose()
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
    nvim.fn.delete(directory, "rf")
  end)
  local text = "phone text"
  local payload = string.char(1) .. text
  local hex = (payload:gsub(".", function(byte)
    return string.format("%02x", byte:byte())
  end))
  local request = {
    v = 1,
    type = "send_prompt",
    controller_id = string.rep("01", 16),
    grant = {
      grant_id = string.rep("02", 16),
      grant_revision = 1,
      projects = { assert(nvim.uv.fs_realpath(root)) },
      agents = { "mock" },
      groups = { "operate_sessions" },
    },
    period = {},
    proof = {},
    request = {
      controller_id = string.rep("01", 16),
      subject_id = subject,
      operation_id = string.rep("03", 16),
      receiver_identity = string.rep("04", 32),
      grant_id = string.rep("02", 16),
      grant_revision = 1,
      period_id = string.rep("05", 16),
      action = 3,
      expected_revision = 1,
      interaction = 1,
      payload = hex,
      payload_hash = nvim.fn.sha256(payload),
    },
  }
  local function read(message)
    local result
    controller:read(nvim.json.encode(message), function(value)
      result = value
    end)
    assert(nvim.wait(5000, function()
      return result ~= nil
    end))
    return result
  end
  local prepared = read(request)
  MiniTest.expect.equality(prepared.type, "action_prepared")
  MiniTest.expect.equality(prompts, 0)
  local commit = {
    v = 1,
    type = "action_commit",
    controller_id = request.controller_id,
    operation_id = request.request.operation_id,
    remaining_ms = 10000,
  }
  local admitted = read(commit)
  MiniTest.expect.equality(admitted.state, "accepted")
  MiniTest.expect.equality(prompts, 1)
  MiniTest.expect.equality(read(commit).code, "unavailable")
  MiniTest.expect.equality(prompts, 1)
  request.request.operation_id = string.rep("06", 16)
  MiniTest.expect.equality(read(request).type, "action_prepared")
  commit.operation_id = request.request.operation_id
  commit.remaining_ms = 0
  local expired = read(commit)
  MiniTest.expect.equality(expired.state, "fenced_no_effect")
  MiniTest.expect.equality(expired.code, "expired")
  MiniTest.expect.equality(prompts, 1)
  request.request.operation_id = string.rep("07", 16)
  MiniTest.expect.equality(read(request).type, "action_prepared")
  assert(controller:detach())
  commit.operation_id = request.request.operation_id
  commit.remaining_ms = 10000
  MiniTest.expect.equality(read(commit).code, "unavailable")
  MiniTest.expect.equality(prompts, 1)
end

return T
