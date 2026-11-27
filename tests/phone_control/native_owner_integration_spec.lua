local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")
local Clock = require("louiselm.phone_control.boot_clock")
local Process = require("louiselm.phone_control.process_identity")
local Session = require("louiselm.session")
local Chat = require("louiselm.ui.chat")

local T = MiniTest.new_set()

T["registered Send enters the real Session once and keeps its later evidence and desktop draft"] = function()
  local root, directory = nvim.fn.getcwd(), nvim.fn.tempname()
  assert(nvim.fn.mkdir(directory, "p") == 1)
  local previous = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local listener, pipe = assert(nvim.uv.new_pipe(false)), assert(nvim.uv.new_pipe(false))
  local peer ---@type table?
  assert(listener:bind(directory .. "/receiver.sock") == 0)
  assert(listener:listen(1, function(err)
    assert(not err)
    peer = assert(nvim.uv.new_pipe(false))
    assert(listener:accept(peer) == 0)
  end) == 0)
  pipe:connect(directory .. "/receiver.sock", function(err)
    assert(not err)
  end)
  assert(nvim.wait(3000, function()
    return peer ~= nil
  end))
  local namespace = assert(nvim.uv.fs_readlink("/proc/self/ns/pid"))
  local process = assert(Process.new(nvim.uv.os_getpid(), namespace))
  local verifier = nvim.env.LOUISELM_TEST_CAPTURE and { nvim.env.LOUISELM_TEST_CAPTURE }
    or { "python3", root .. "/tests/fixtures/phone_clock_verifier.py", "healthy" }
  local clock = Clock.new(verifier)
  local function open(object)
    local done
    object:open(function(ok)
      done = ok
    end)
    assert(nvim.wait(3000, function()
      return done ~= nil
    end))
    assert(done)
  end
  open(process)
  open(clock)
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
  local failing_definition = nvim.deepcopy(definition)
  failing_definition.env.LOUISELM_MOCK_MODE = "crash"
  local hold_definition = nvim.deepcopy(definition)
  hold_definition.env.LOUISELM_MOCK_MODE = "hold"
  local api = assert(Session.new({ mock = definition, failing = failing_definition, hold = hold_definition }, nil, {
    usage_directory = directory .. "/usage",
  }))
  local controller = Controller.new({
    service = { nvim.fn.exepath("python3"), root .. "/tests/fixtures/phone_control_owner_stub.py" },
  })
  local chat
  MiniTest.finally(function()
    if chat then
      chat:dispose()
    end
    controller:dispose()
    assert(api:dispose())
    local done = false
    api:flush_recording(function()
      done = true
    end)
    assert(nvim.wait(6000, function()
      return done
    end))
    process:dispose()
    clock:dispose()
    for _, handle in ipairs({ listener, pipe, peer }) do
      if not handle:is_closing() then
        handle:close()
      end
    end
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
    nvim.fn.delete(directory, "rf")
  end)
  local session = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end))
  chat = assert(Chat.new(api, { agents = { "mock" }, start_insert_on_switch = false }))
  assert(chat:attach(session))
  assert(chat:queue_context({ label = "private", text = "UNSENT CONTEXT" }))
  local view = chat.views[session:inspect().id]
  view.renderer:replace_prompt("UNSENT DRAFT")
  local controller_id = string.rep("01", 16)
  assert(
    controller:attach(
      controller_id,
      { pipe = pipe, process = process, clock = clock, generation = string.rep("09", 16) }
    )
  )
  local subject = assert(controller:track(session))
  local text, operation = "Reply from phone", string.rep("02", 16)
  local payload = string.char(1) .. text
  local request = {
    controller_id = controller_id,
    subject_id = subject,
    operation_id = operation,
    receiver_identity = string.rep("03", 32),
    grant_id = string.rep("04", 16),
    grant_revision = 1,
    period_id = string.rep("05", 16),
    action = 3,
    interaction = 1,
    expected_revision = controller.subjects[subject].projection.revision,
    payload = (payload:gsub(".", function(byte)
      return string.format("%02x", byte:byte())
    end)),
    payload_hash = nvim.fn.sha256(payload),
  }
  local message = {
    v = 1,
    type = "send_prompt",
    controller_id = controller_id,
    grant = { projects = { assert(nvim.uv.fs_realpath(root)) }, agents = { "mock" }, groups = { "operate_sessions" } },
    period = {},
    proof = {},
    request = request,
  }
  assert(nvim.fn.writefile({}, directory .. "/hold-completion") == 0)
  assert(nvim.fn.writefile({ operation }, directory .. "/fail-once-completed") == 0)
  local function read(value)
    local result
    controller:read(nvim.json.encode(value), function(response)
      result = response
    end)
    assert(nvim.wait(6000, function()
      return result ~= nil
    end))
    return result
  end
  MiniTest.expect.equality(read(message).type, "action_prepared")
  local witness = ""
  assert(peer):read_start(function(err, data)
    assert(not err)
    if data then
      witness = witness .. data
    end
  end)
  local outcome = read({
    v = 2,
    type = "action_commit",
    controller_id = controller_id,
    operation_id = operation,
    gate_id = string.rep("06", 16),
    remaining_ms = 5000,
  })
  MiniTest.expect.equality(outcome.state, "accepted")
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready" and nvim.uv.fs_stat(directory .. "/completion-started") ~= nil
  end))
  assert(nvim.wait(3000, function()
    return witness:find("\n", 1, true) ~= nil
  end))
  assert(controller:detach())
  pipe:close()
  assert(peer):close()
  pipe, peer = assert(nvim.uv.new_pipe(false)), nil
  local reconnected
  pipe:connect(directory .. "/receiver.sock", function(err)
    assert(not err)
    reconnected = true
  end)
  assert(nvim.wait(3000, function()
    return reconnected and peer ~= nil
  end))
  assert(peer):read_start(function(err, data)
    assert(not err)
    if data then
      witness = witness .. data
    end
  end)
  controller_id = string.rep("11", 16)
  assert(
    controller:attach(
      controller_id,
      { pipe = pipe, process = process, clock = clock, generation = string.rep("09", 16) }
    )
  )
  subject = assert(controller:track(session))
  message.controller_id, request.controller_id, request.subject_id = controller_id, controller_id, subject
  request.operation_id = string.rep("07", 16)
  request.expected_revision = controller.subjects[subject].projection.revision
  MiniTest.expect.equality(read(message).type, "action_prepared")
  local blocked = read({
    v = 2,
    type = "action_commit",
    controller_id = controller_id,
    operation_id = request.operation_id,
    gate_id = string.rep("08", 16),
    remaining_ms = 5000,
  })
  MiniTest.expect.equality(blocked.state, "fenced_no_effect")
  MiniTest.expect.equality(session:inspect().current_turn, 1)
  local other_session = assert(api:create_session("mock", { cwd = root }))
  assert(nvim.wait(6000, function()
    return other_session:inspect().status == "ready"
  end))
  local other_subject = assert(controller:track(other_session))
  local other_message = nvim.deepcopy(message)
  other_message.request.subject_id = other_subject
  other_message.request.operation_id = string.rep("0a", 16)
  other_message.request.expected_revision = controller.subjects[other_subject].projection.revision
  MiniTest.expect.equality(read(other_message).type, "action_prepared")
  local independent = read({
    v = 2,
    type = "action_commit",
    controller_id = controller_id,
    operation_id = other_message.request.operation_id,
    gate_id = string.rep("0b", 16),
    remaining_ms = 5000,
  })
  MiniTest.expect.equality(independent.state, "accepted")
  MiniTest.expect.equality(other_session:inspect().current_turn, 1)
  assert(nvim.fn.delete(directory .. "/hold-completion") == 0)
  assert(nvim.wait(6000, function()
    local path = directory .. "/owner-reports.jsonl"
    if not nvim.uv.fs_stat(path) or not nvim.uv.fs_stat(directory .. "/owner-failures") then
      return false
    end
    if not table.concat(nvim.fn.readfile(directory .. "/owner-failures"), "\n"):find(operation, 1, true) then
      return false
    end
    for _, line in ipairs(nvim.fn.readfile(path)) do
      local report = nvim.json.decode(line)
      if report.identity.operation_id == operation and type(report.outcome) == "table" then
        if report.outcome.completed ~= nil then
          return true
        end
      end
    end
    return false
  end))
  local records = {}
  for _, line in ipairs(nvim.fn.readfile(directory .. "/owner-reports.jsonl")) do
    local report = nvim.json.decode(line)
    if report.outcome and report.identity.operation_id == operation then
      records[#records + 1] = report.outcome
    end
  end
  MiniTest.expect.equality(#records, 3)
  MiniTest.expect.equality(type(records[1].accepted), "table")
  MiniTest.expect.equality(type(records[2].dispatched), "table")
  MiniTest.expect.equality(type(records[3].completed), "table")
  local failing = assert(api:create_session("failing", { cwd = root }))
  assert(nvim.wait(6000, function()
    return failing:inspect().status == "ready"
  end))
  local failed_subject = assert(controller:track(failing))
  local failed_message = nvim.deepcopy(message)
  failed_message.grant.agents = { "failing" }
  failed_message.request.subject_id = failed_subject
  failed_message.request.operation_id = string.rep("0c", 16)
  failed_message.request.expected_revision = controller.subjects[failed_subject].projection.revision
  MiniTest.expect.equality(read(failed_message).type, "action_prepared")
  local failed = read({
    v = 2,
    type = "action_commit",
    controller_id = controller_id,
    operation_id = failed_message.request.operation_id,
    gate_id = string.rep("0d", 16),
    remaining_ms = 5000,
  })
  MiniTest.expect.equality(failed.state, "accepted")
  assert(nvim.wait(6000, function()
    return controller.uncertain_sessions[failing] == true
  end))
  assert(controller:detach())
  assert(
    controller:attach(
      string.rep("12", 16),
      { pipe = pipe, process = process, clock = clock, generation = string.rep("09", 16) }
    )
  )
  MiniTest.expect.equality(controller.uncertain_sessions[failing], true)

  local cancelled = assert(api:create_session("hold", { cwd = root }))
  assert(nvim.wait(6000, function()
    return cancelled:inspect().status == "ready"
  end))
  local dispatched = false
  local unsubscribe = cancelled:on(function(event)
    if event.type == "prompt_dispatched" then
      dispatched = true
    end
  end)
  MiniTest.finally(unsubscribe)
  local cancelled_subject = assert(controller:track(cancelled))
  local cancelled_message = nvim.deepcopy(message)
  cancelled_message.controller_id = controller.controller_id
  cancelled_message.grant.agents = { "hold" }
  cancelled_message.request.controller_id = controller.controller_id
  cancelled_message.request.subject_id = cancelled_subject
  cancelled_message.request.operation_id = string.rep("0e", 16)
  cancelled_message.request.expected_revision = controller.subjects[cancelled_subject].projection.revision
  MiniTest.expect.equality(read(cancelled_message).type, "action_prepared")
  local cancelled_outcome = read({
    v = 2,
    type = "action_commit",
    controller_id = controller.controller_id,
    operation_id = cancelled_message.request.operation_id,
    gate_id = string.rep("0f", 16),
    remaining_ms = 5000,
  })
  MiniTest.expect.equality(cancelled_outcome.state, "accepted")
  assert(nvim.wait(6000, function()
    return dispatched
  end))
  assert(cancelled:cancel())
  assert(nvim.wait(6000, function()
    return controller.uncertain_sessions[cancelled] == true
  end))
  local cancelled_phases = {}
  for _, line in ipairs(nvim.fn.readfile(directory .. "/owner-reports.jsonl")) do
    local report = nvim.json.decode(line)
    if report.outcome and report.identity.operation_id == cancelled_message.request.operation_id then
      cancelled_phases[#cancelled_phases + 1] = report.outcome
    end
  end
  MiniTest.expect.equality(#cancelled_phases, 3)
  MiniTest.expect.equality(type(cancelled_phases[1].accepted), "table")
  MiniTest.expect.equality(type(cancelled_phases[2].dispatched), "table")
  MiniTest.expect.equality(cancelled_phases[3], "unknown")

  local fenced = nvim.deepcopy(message)
  fenced.controller_id = controller.controller_id
  fenced.request.controller_id = controller.controller_id
  fenced.request.subject_id = assert(controller:track(session))
  fenced.request.operation_id = string.rep("13", 16)
  fenced.request.expected_revision = controller.subjects[fenced.request.subject_id].projection.revision
  MiniTest.expect.equality(read(fenced).type, "action_prepared")
  assert(nvim.fn.writefile({ fenced.request.operation_id }, directory .. "/fail-once-fenced_no_effect") == 0)
  assert(nvim.fn.writefile({}, directory .. "/hold-fence") == 0)
  assert(controller:detach())
  assert(nvim.wait(6000, function()
    return nvim.uv.fs_stat(directory .. "/fence-started") ~= nil
  end))
  for _, line in ipairs(nvim.fn.readfile(directory .. "/owner-reports.jsonl")) do
    local report = nvim.json.decode(line)
    MiniTest.expect.equality(
      report.identity.operation_id == fenced.request.operation_id and report.outcome ~= nil,
      false
    )
  end
  assert(controller:attach(string.rep("14", 16), {
    pipe = pipe,
    process = process,
    clock = clock,
    generation = string.rep("09", 16),
  }))
  assert(nvim.fn.delete(directory .. "/hold-fence") == 0)
  assert(nvim.wait(6000, function()
    if
      not table
        .concat(nvim.fn.readfile(directory .. "/owner-failures"), "\n")
        :find(fenced.request.operation_id, 1, true)
    then
      return false
    end
    for _, line in ipairs(nvim.fn.readfile(directory .. "/owner-reports.jsonl")) do
      local report = nvim.json.decode(line)
      if report.identity.operation_id == fenced.request.operation_id and report.outcome then
        return report.outcome.fenced_no_effect ~= nil
      end
    end
    return false
  end))
  MiniTest.expect.equality(
    read({
      v = 2,
      type = "action_commit",
      controller_id = fenced.controller_id,
      operation_id = fenced.request.operation_id,
      gate_id = string.rep("15", 16),
      remaining_ms = 5000,
    }).code,
    "unavailable"
  )
  MiniTest.expect.equality(session:inspect().current_turn, 1)
  local witnesses = {}
  for line in witness:gmatch("([^\n]+)\n") do
    witnesses[#witnesses + 1] = nvim.json.decode(line)
  end
  MiniTest.expect.equality(#witnesses, 4)
  MiniTest.expect.equality(witnesses[1].operation_id, operation)
  MiniTest.expect.equality(witnesses[2].operation_id, other_message.request.operation_id)
  MiniTest.expect.equality(witnesses[3].operation_id, failed_message.request.operation_id)
  MiniTest.expect.equality(witnesses[4].operation_id, cancelled_message.request.operation_id)
  MiniTest.expect.equality(session:inspect().current_turn, 1)
  MiniTest.expect.equality(view.renderer:prompt_text(), "UNSENT DRAFT")
  MiniTest.expect.equality(
    table.concat(nvim.api.nvim_buf_get_lines(view.renderer.buffer, 0, -1, false), "\n"):find("UNSENT CONTEXT", 1, true),
    nil
  )
end

return T
