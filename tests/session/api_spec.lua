local MiniTest = require("mini.test")
local Protocol = require("louiselm.acp.protocol")
local Permission = require("louiselm.permission")
local Session = require("louiselm.session")

local T = MiniTest.new_set()

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local function fake_processes()
  local processes = {}
  local original_system = nvim.system
  nvim.system = function(command, options, on_exit)
    local process = {
      command = command,
      options = options,
      on_exit = on_exit,
      writes = {},
      closed = false,
    }
    process.handle = {
      write = function(_, data)
        process.writes[#process.writes + 1] = data
      end,
      kill = function()
        process.closed = true
      end,
      is_closing = function()
        return process.closed
      end,
    }
    processes[#processes + 1] = process
    return process.handle
  end
  return processes, original_system
end

local function restore_processes(original_system)
  nvim.system = original_system
end

local function respond(process, id, result)
  local message = Protocol.response(id, result)
  local encoded = assert(Protocol.encode(message))
  process.options.stdout(nil, encoded .. "\n")
end

local function notification(process, method, params)
  local message = assert(Protocol.notification(method, params))
  local encoded = assert(Protocol.encode(message))
  process.options.stdout(nil, encoded .. "\n")
end

local function start_ready_session(api, processes, name, cwd)
  local ready
  local session = assert(api:create_session(name, { cwd = cwd }, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = name .. "-acp" })
  MiniTest.expect.equality(ready.error, nil)
  MiniTest.expect.equality(ready.session, session)
  return session, process
end

T["new"] = MiniTest.new_set()

T["new"]["creates concurrent addressable sessions and exposes state"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    one = { command = "agent-one", args = {} },
    two = { command = "agent-two", args = {} },
  }))

  local first = start_ready_session(api, processes, "one", "/tmp/one")
  local second = start_ready_session(api, processes, "two", "/tmp/two")

  MiniTest.expect.equality(first:inspect(), {
    id = "session-1",
    agent = "one",
    status = "ready",
    working_dir = "/tmp/one",
    current_turn = 0,
  })
  MiniTest.expect.equality(second:inspect(), {
    id = "session-2",
    agent = "two",
    status = "ready",
    working_dir = "/tmp/two",
    current_turn = 0,
  })
  MiniTest.expect.equality(api:list_sessions(), { "session-1", "session-2" })
  MiniTest.expect.equality(api:get_session("session-1"), first)
  MiniTest.expect.equality(api:get_session("session-2"), second)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["emits typed streamed events and completes a prompt"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  local completed
  local request_id = assert(session:prompt("hello", function(result, err)
    completed = { result = result, error = err }
  end))
  MiniTest.expect.equality(request_id, 3)
  MiniTest.expect.equality(session:inspect().status, "prompting")

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "agent_message_chunk",
      content = { type = "text", text = "hello" },
    },
  })
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "tool_call",
      toolCallId = "tool-1",
      status = "in_progress",
    },
  })
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "tool_call_update",
      toolCallId = "tool-1",
      status = "completed",
    },
  })
  respond(process, request_id, { stopReason = "end_turn" })

  MiniTest.expect.equality({ events[1].type, events[2].type, events[3].type, events[4].type }, {
    "chunk",
    "tool_call_started",
    "tool_call_finished",
    "turn_done",
  })
  MiniTest.expect.equality(events[1].data.content.text, "hello")
  MiniTest.expect.equality(completed, { result = { stopReason = "end_turn" }, error = nil })
  MiniTest.expect.equality(session:inspect().status, "ready")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["cancels and disposes without allowing late process results"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")

  assert(session:prompt("hello"))
  local sent, cancel_error = session:cancel()
  MiniTest.expect.equality(sent, true)
  MiniTest.expect.equality(cancel_error, nil)
  MiniTest.expect.equality(session:inspect().status, "cancelling")
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).method, "session/cancel")

  session:dispose()
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(api:get_session("session-1"), nil)
  process.on_exit({ code = 1, signal = 0, stdout = "", stderr = "crashed" })
  MiniTest.expect.equality(session:inspect().status, "disposed")

  restore_processes(original_system)
end

T["new"]["publishes permission requests with a response function"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)

  local request = {
    jsonrpc = "2.0",
    id = 9,
    method = "session/request_permission",
    params = { sessionId = "agent-acp", options = { "allow", "deny" } },
  }
  process.options.stdout(nil, assert(Protocol.encode(request)) .. "\n")

  MiniTest.expect.equality(permission.data.options, { "allow", "deny" })
  MiniTest.expect.equality(permission.data.request_id, 9)
  local sent, send_error = permission.respond({ outcome = { outcome = "cancelled" } })
  MiniTest.expect.equality(sent, true)
  MiniTest.expect.equality(send_error, nil)
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).id, 9)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["automatically responds only to explicitly scoped permission requests"] = function()
  local processes, original_system = fake_processes()
  local policy = assert(Permission.policy("auto-approve-scoped", { paths = { "/tmp/project" } }))
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session = assert(api:create_session("agent", {
    cwd = "/tmp/project",
    permission_policy = policy,
  }))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "agent-acp" })
  local permission_event
  session:on(function(event)
    if event.type == "permission_requested" then
      permission_event = event
    end
  end)

  local request = {
    jsonrpc = "2.0",
    id = 9,
    method = "session/request_permission",
    params = {
      sessionId = "agent-acp",
      toolCall = { kind = "edit", rawInput = { path = "/tmp/project/init.lua" } },
      options = { { optionId = "allow-once", kind = "allow_once" } },
    },
  }
  process.options.stdout(nil, assert(Protocol.encode(request)) .. "\n")

  MiniTest.expect.equality(permission_event, nil)
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).result, {
    outcome = { outcome = "selected", optionId = "allow-once" },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["calls a prompt callback with the error when the agent crashes"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local completion
  assert(session:prompt("hello", function(result, err)
    completion = { result = result, error = err }
  end))

  process.on_exit({ code = 23, signal = 0, stdout = "", stderr = "boom" })

  MiniTest.expect.equality(completion.result, nil)
  MiniTest.expect.equality(completion.error, "agent process exited with code 23")
  MiniTest.expect.equality(session:inspect().status, "error")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["turns an unexpected agent exit into a session error event"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local event
  session:on(function(value)
    if value.type == "error" then
      event = value
    end
  end)

  process.on_exit({ code = 23, signal = 0, stdout = "", stderr = "boom" })

  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(event.session_id, "session-1")
  MiniTest.expect.equality(event.data.message, "agent process exited with code 23")

  api:dispose()
  restore_processes(original_system)
end

return T
