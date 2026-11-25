local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")
local T = MiniTest.new_set()

T["the unguarded version-1 Send is unsupported before owner reservation"] = function()
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
  local controller = Controller.new({ service = { "/nonexistent-capture" } })
  require("tests.fixtures.phone_control_history")(session, controller)
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
  MiniTest.expect.equality(read(request), { v = 1, type = "error", code = "unsupported" })
  MiniTest.expect.equality(prompts, 0)
  MiniTest.expect.equality(nvim.uv.fs_stat(directory), nil)
  local commit = {
    v = 1,
    type = "action_commit",
    controller_id = request.controller_id,
    operation_id = request.request.operation_id,
    remaining_ms = 10000,
  }
  MiniTest.expect.equality(read(commit), { v = 1, type = "error", code = "unavailable" })
  assert(controller:detach())
  MiniTest.expect.equality(read(commit).code, "unavailable")
  MiniTest.expect.equality(prompts, 0)
end

return T
