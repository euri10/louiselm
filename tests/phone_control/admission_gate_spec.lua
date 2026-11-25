local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Gate = require("louiselm.phone_control.admission_gate")
local Clock = require("louiselm.phone_control.boot_clock")
local Process = require("louiselm.phone_control.process_identity")
local Lifecycle = require("louiselm.session.lifecycle")
local Sessions = require("louiselm.session")
local Projection = require("louiselm.phone_control.projection")

local T = MiniTest.new_set()

local function pair(path)
  local listener = assert(nvim.uv.new_pipe(false))
  local client = assert(nvim.uv.new_pipe(false))
  local peer
  assert(listener:bind(path) == 0)
  assert(listener:listen(1, function(err)
    assert(not err)
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
  end) == 0)
  client:connect(path, function(err)
    assert(not err)
  end)
  assert(nvim.wait(5000, function()
    return peer ~= nil
  end))
  return listener, client, peer
end

local function opened(object)
  local result
  object:open(function(ok)
    result = ok
  end)
  assert(nvim.wait(5000, function()
    return result ~= nil
  end))
  assert(result)
end

local function fixture(receiver_pid, guard_pid)
  local directory = nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  local gate_listener, gate, gate_peer = pair(directory .. "/gate")
  local controller_listener, controller, controller_peer = pair(directory .. "/controller")
  local namespace = assert(nvim.uv.fs_readlink("/proc/self/ns/pid"))
  local guard_process = assert(Process.new(guard_pid or nvim.uv.os_getpid(), namespace))
  local receiver_process = assert(Process.new(receiver_pid or nvim.uv.os_getpid(), namespace))
  local clock = Clock.new()
  MiniTest.finally(function()
    guard_process:dispose()
    receiver_process:dispose()
    clock:dispose()
    for _, pipe in ipairs({ gate_listener, gate, gate_peer, controller_listener, controller, controller_peer }) do
      if not pipe:is_closing() then
        pipe:close()
      end
    end
    nvim.fn.delete(directory, "rf")
  end)
  opened(guard_process)
  opened(receiver_process)
  opened(clock)
  local start = assert(clock:sample())
  local options = {
    gate = gate,
    controller = controller,
    guard_process = guard_process,
    receiver_process = receiver_process,
    clock = clock,
    started = start,
    remaining_ms = 5000,
    session = {},
    projection = { revision = 1 },
    expected_revision = 1,
    payload_hash = nvim.fn.sha256(string.char(1) .. "phone"),
    controller_id = string.rep("01", 16),
    operation_id = string.rep("02", 16),
    gate_id = string.rep("03", 16),
  }
  return options, gate_peer, controller_peer
end

T["a one-use check writes exact operation witnesses without yielding"] = function()
  local options, gate_peer, controller_peer = fixture()
  local messages = {}
  for index, peer in ipairs({ gate_peer, controller_peer }) do
    peer:read_start(function(err, data)
      assert(not err)
      if data ~= nil then
        messages[index] = (messages[index] or "") .. data
      end
    end)
  end
  local gate = assert(Gate.new(options))
  local scheduled = false
  nvim.schedule(function()
    scheduled = true
  end)
  options.started.namespace = "invalid"
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), true)
  MiniTest.expect.equality(scheduled, false)
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
  assert(nvim.wait(5000, function()
    return messages[1] ~= nil
      and messages[1]:find("\n", 1, true) ~= nil
      and messages[2] ~= nil
      and messages[2]:find("\n", 1, true) ~= nil
  end))
  for index, kind in ipairs({ "gate_use", "action_witness" }) do
    MiniTest.expect.equality(nvim.json.decode(messages[index]), {
      v = 2,
      type = kind,
      controller_id = options.controller_id,
      operation_id = options.operation_id,
      gate_id = options.gate_id,
    })
  end
end

T["a cut gate cannot yield a successful check"] = function()
  local options, peer = fixture()
  local closed = false
  peer:close(function()
    closed = true
  end)
  assert(nvim.wait(5000, function()
    return closed
  end))
  local gate = assert(Gate.new(options))
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
end

T["a dead receiver fails despite its original socket remaining writable"] = function()
  local exited = false
  local child = nvim.system({ "sleep", "30" }, {}, function()
    exited = true
  end)
  MiniTest.finally(function()
    if not exited then
      child:kill(9)
    end
  end)
  local options = fixture(child.pid)
  local gate = assert(Gate.new(options))
  child:kill(9)
  assert(nvim.wait(5000, function()
    return exited
  end))
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
end

T["a dead guard fails despite its gate socket remaining writable"] = function()
  local exited = false
  local child = nvim.system({ "sleep", "30" }, {}, function()
    exited = true
  end)
  MiniTest.finally(function()
    if not exited then
      child:kill(9)
    end
  end)
  local options = fixture(nil, child.pid)
  local gate = assert(Gate.new(options))
  child:kill(9)
  assert(nvim.wait(5000, function()
    return exited
  end))
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
end

T["an elapsed final interval refuses admission"] = function()
  local options = fixture()
  options.started.centiseconds = options.started.centiseconds - 500
  local gate = assert(Gate.new(options))
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
end

T["the check never invokes caller-provided identity or gate callbacks"] = function()
  local options = fixture()
  local called = false
  options.guard_process = {
    alive = function()
      called = true
      return true
    end,
  }
  local gate = assert(Gate.new(options))
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
  MiniTest.expect.equality(
    Gate.check({
      check = function()
        called = true
      end,
    }, options.session, "phone"),
    false
  )
  MiniTest.expect.equality(called, false)
end

T["the real Session checks its gate and reserves admission before observers reenter"] = function()
  local root, directory = nvim.fn.getcwd(), nvim.fn.tempname()
  local api = assert(Sessions.new({
    mock = {
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
    },
  }, nil, { usage_directory = directory }))
  MiniTest.finally(function()
    assert(api:dispose())
    local finished = false
    api:flush_recording(function()
      finished = true
    end)
    assert(nvim.wait(6000, function()
      return finished
    end))
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local projection = Projection.new(function()
    return session:inspect().status
  end)
  local nested, attempted = nil, false
  local unsubscribe = session:on(function(event)
    projection:record(event)
    if event.type == "state_changed" and event.data.status == "preparing" then
      attempted = true
      nested = session:prompt("desktop")
    end
  end)
  MiniTest.finally(unsubscribe)
  local invalid = fixture()
  invalid.session, invalid.projection, invalid.expected_revision = session, projection, projection.revision
  local invalid_gate = assert(Gate.new(invalid))
  local denied, denial = Lifecycle.prompt_phone(session, invalid_gate, "altered")
  MiniTest.expect.equality(denied, nil)
  MiniTest.expect.equality(denial, "phone authority unavailable")
  MiniTest.expect.equality(session:inspect().current_turn, 0)
  MiniTest.expect.equality(session:inspect().status, "ready")

  local cut, cut_peer = fixture()
  cut.session, cut.projection, cut.expected_revision = session, projection, projection.revision
  local cut_gate = assert(Gate.new(cut))
  local closed = false
  cut_peer:close(function()
    closed = true
  end)
  assert(nvim.wait(5000, function()
    return closed
  end))
  denied, denial = Lifecycle.prompt_phone(session, cut_gate, "phone")
  MiniTest.expect.equality(denied, nil)
  MiniTest.expect.equality(denial, "phone authority unavailable")
  MiniTest.expect.equality(session:inspect().current_turn, 0)
  MiniTest.expect.equality(session:inspect().status, "ready")

  local options = fixture()
  options.session, options.projection, options.expected_revision = session, projection, projection.revision
  local gate = assert(Gate.new(options))
  local turn = Lifecycle.prompt_phone(session, gate, "phone")
  MiniTest.expect.equality(type(turn), "string")
  MiniTest.expect.equality(attempted, true)
  MiniTest.expect.equality(nested, nil)
  MiniTest.expect.equality(session:inspect().current_turn, 1)
  MiniTest.expect.equality(Gate.check(gate, session, "phone"), false)

  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  local pending = fixture()
  pending.session, pending.projection, pending.expected_revision = session, projection, projection.revision
  local pending_gate = assert(Gate.new(pending))
  local desktop = session:prompt("desktop first")
  MiniTest.expect.equality(type(desktop), "string")
  denied, denial = Lifecycle.prompt_phone(session, pending_gate, "phone")
  MiniTest.expect.equality(denied, nil)
  MiniTest.expect.equality(denial, "session is not ready")
  MiniTest.expect.equality(session:inspect().current_turn, 2)
  MiniTest.expect.equality(Gate.check(pending_gate, session, "phone"), false)
end

T["a stale Session revision cannot pass the phone gate"] = function()
  local options = fixture()
  options.session = {}
  options.projection = { revision = 2 }
  options.expected_revision = 1
  local gate = assert(Gate.new(options))
  MiniTest.expect.equality(Gate.check(gate, options.session, "phone"), false)
end

return T
