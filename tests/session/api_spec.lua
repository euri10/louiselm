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
  rawset(nvim, "system", function(command, options, on_exit)
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
  end)
  return processes, original_system
end

local function restore_processes(original_system)
  rawset(nvim, "system", original_system)
end

local function respond(process, id, result)
  local message = Protocol.response(id, result)
  local encoded = assert(Protocol.encode(message))
  process.options.stdout(nil, encoded .. "\n")
end

local function respond_error(process, id, rpc_error)
  local encoded = assert(Protocol.encode({ jsonrpc = "2.0", id = id, error = rpc_error }))
  process.options.stdout(nil, encoded .. "\n")
end

local function notification(process, method, params)
  local message = assert(Protocol.notification(method, params))
  local encoded = assert(Protocol.encode(message))
  process.options.stdout(nil, encoded .. "\n")
end

local function permission_request(process, acp_session_id, id, options)
  local message = {
    jsonrpc = "2.0",
    id = id,
    method = "session/request_permission",
    params = { sessionId = acp_session_id, options = options },
  }
  process.options.stdout(nil, assert(Protocol.encode(message)) .. "\n")
end

---@return { id: string|number, outcome: unknown }[] outcomes One entry per permission response written to the agent.
local function permission_outcomes(process)
  local outcomes = {}
  for _, write in ipairs(process.writes) do
    local message = assert(Protocol.decode(write:sub(1, -2)))
    if type(message.result) == "table" and type(message.result.outcome) == "table" then
      outcomes[#outcomes + 1] = { id = message.id, outcome = message.result.outcome.outcome }
    end
  end
  return outcomes
end

local function start_ready_session(api, processes, name, cwd)
  local ready
  local session = assert(api:create_session(name, { cwd = cwd }, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params, {
    cwd = cwd,
    mcpServers = {},
  })
  respond(process, 2, { sessionId = name .. "-acp" })
  MiniTest.expect.equality(ready.error, nil)
  MiniTest.expect.equality(ready.session, session)
  return session, process
end

T["new"] = MiniTest.new_set()

T["new"]["loads an existing ACP session and receives replayed history"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local ready
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session = assert(api:load_session("agent", "prior-acp", {
    cwd = "/tmp/project",
    on_event = function(event)
      events[#events + 1] = event
    end,
  }, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))), {
    id = 2,
    jsonrpc = "2.0",
    method = "session/load",
    params = { sessionId = "prior-acp", cwd = "/tmp/project", mcpServers = {} },
  })
  notification(process, "session/update", {
    sessionId = "prior-acp",
    update = {
      sessionUpdate = "agent_message_chunk",
      content = { type = "text", text = "previous answer" },
    },
  })
  notification(process, "session/update", {
    sessionId = "prior-acp",
    update = {
      sessionUpdate = "config_option_update",
      configOptions = {
        { id = "brave", name = "Brave", type = "boolean", currentValue = true },
      },
    },
  })
  respond(process, 2, {})

  MiniTest.expect.equality(ready.error, nil)
  MiniTest.expect.equality(ready.session, session)
  MiniTest.expect.equality(events[1].data.content.text, "previous answer")
  MiniTest.expect.equality(session:inspect().acp_session_id, "prior-acp")
  MiniTest.expect.equality(session:inspect().config_options[1].current_value, true)
  MiniTest.expect.equality(session:inspect().status, "ready")

  assert(api:dispose())
  restore_processes(original_system)
end

T["new"]["replays the user's own prior messages as user_chunk events when loading a session"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session = assert(api:load_session("agent", "prior-acp", {
    cwd = "/tmp/project",
    on_event = function(event)
      events[#events + 1] = event
    end,
  }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  notification(process, "session/update", {
    sessionId = "prior-acp",
    update = {
      sessionUpdate = "user_message_chunk",
      content = { type = "text", text = "what did we decide last time" },
    },
  })
  notification(process, "session/update", {
    sessionId = "prior-acp",
    update = {
      sessionUpdate = "agent_message_chunk",
      content = { type = "text", text = "previous answer" },
    },
  })
  respond(process, 2, {})

  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality({ events[1].type, events[2].type }, { "user_chunk", "chunk" })
  MiniTest.expect.equality(events[1].data.content.text, "what did we decide last time")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["closes the ACP process when loading returns a malformed result"] = function()
  local processes, original_system = fake_processes()
  local ready
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session = assert(api:load_session("agent", "prior-acp", nil, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond(process, 2, nvim.NIL)

  MiniTest.expect.equality(ready.session, nil)
  MiniTest.expect.equality(ready.error, "ACP session/load returned a malformed result")
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(process.closed, true)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["explains a Codex active-writer load failure without exposing its thread id"] = function()
  local processes, original_system = fake_processes()
  local ready
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session = assert(api:load_session("agent", "prior-acp", nil, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond_error(process, 2, {
    code = -32603,
    message = "Internal error",
    data = { details = "thread sensitive-thread-id already has an active writer" },
  })

  MiniTest.expect.equality(ready.session, nil)
  MiniTest.expect.equality(
    ready.error,
    "ACP session/load failed: session is already open in another client; close it there before resuming"
  )
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(process.closed, true)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["does not expose arbitrary internal ACP error details"] = function()
  local processes, original_system = fake_processes()
  local ready_error
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  assert(api:load_session("agent", "prior-acp", nil, function(_, err)
    ready_error = err
  end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond_error(process, 2, {
    code = -32603,
    message = "Internal error",
    data = { details = "secret adapter context" },
  })

  MiniTest.expect.equality(ready_error, "ACP session/load failed: Internal error")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["discovers paginated sessions with adapter-scoped identities"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    claude = { command = "claude-agent", args = {} },
    codex = { command = "codex-agent", args = {} },
  }))
  local discovered
  local discovery_errors

  local started, start_error = api:discover_sessions(nil, function(sessions, errors)
    discovered = sessions
    discovery_errors = errors
  end)
  MiniTest.expect.equality({ started, start_error }, { true, nil })
  MiniTest.expect.equality(#processes, 2)

  local by_command = {}
  for _, process in ipairs(processes) do
    by_command[process.command[1]] = process
  end
  local claude = by_command["claude-agent"]
  local codex = by_command["codex-agent"]
  respond(claude, 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  MiniTest.expect.equality(assert(Protocol.decode(claude.writes[2]:sub(1, -2))).params, {})
  respond(claude, 2, {
    sessions = {
      { sessionId = "shared", cwd = "/tmp/one", title = "Older", updatedAt = "2026-08-08T10:00:00Z" },
    },
    nextCursor = "page-2",
  })
  MiniTest.expect.equality(assert(Protocol.decode(claude.writes[3]:sub(1, -2))).params, { cursor = "page-2" })
  respond(claude, 3, {
    sessions = {
      { sessionId = "shared", cwd = "/tmp/two", title = "Newest", updatedAt = "2026-08-10T10:00:00Z" },
    },
  })
  MiniTest.expect.equality(discovered, nil)

  respond(codex, 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  respond(codex, 2, {
    sessions = {
      { sessionId = "shared", cwd = "/tmp/one", title = nvim.NIL, updatedAt = nvim.NIL },
    },
    nextCursor = nvim.NIL,
  })

  MiniTest.expect.equality(discovery_errors, {})
  MiniTest.expect.equality(discovered, {
    {
      agent = "claude",
      session_id = "shared",
      cwd = "/tmp/two",
      title = "Newest",
      updated_at = "2026-08-10T10:00:00Z",
    },
    {
      agent = "claude",
      session_id = "shared",
      cwd = "/tmp/one",
      title = "Older",
      updated_at = "2026-08-08T10:00:00Z",
    },
    {
      agent = "codex",
      session_id = "shared",
      cwd = "/tmp/one",
    },
  })

  assert(api:dispose())
  restore_processes(original_system)
end

T["new"]["filters discovery to one workspace and reports unsupported adapters"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    supported = { command = "supported-agent", args = {} },
    unsupported = { command = "unsupported-agent", args = {} },
  }))
  local discovered
  local discovery_errors

  assert(api:discover_sessions({ cwd = "/tmp/project" }, function(sessions, errors)
    discovered = sessions
    discovery_errors = errors
  end))
  local by_command = {}
  for _, process in ipairs(processes) do
    by_command[process.command[1]] = process
  end
  local supported = by_command["supported-agent"]
  local unsupported = by_command["unsupported-agent"]
  respond(supported, 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  MiniTest.expect.equality(assert(Protocol.decode(supported.writes[2]:sub(1, -2))).params, { cwd = "/tmp/project" })
  respond(supported, 2, {
    sessions = {
      { sessionId = "keep", cwd = "/tmp/project" },
      { sessionId = "drop", cwd = "/tmp/other" },
    },
  })
  respond(unsupported, 1, { protocolVersion = 1, agentCapabilities = {} })

  MiniTest.expect.equality(discovered, {
    { agent = "supported", session_id = "keep", cwd = "/tmp/project" },
  })
  MiniTest.expect.equality(discovery_errors, {
    { agent = "unsupported", message = "ACP agent does not support session/list" },
  })

  assert(api:dispose())
  restore_processes(original_system)
end

T["new"]["rejects malformed discovery and ignores late results after disposal"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local callbacks = 0
  local discovered
  local discovery_errors

  local started, start_error = api:discover_sessions({ cwd = "relative/project" }, function() end)
  MiniTest.expect.equality({ started, start_error }, { false, "discovery cwd must be an absolute path" })

  assert(api:discover_sessions(nil, function(sessions, errors)
    callbacks = callbacks + 1
    discovered = sessions
    discovery_errors = errors
  end))
  respond(processes[1], 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  respond(processes[1], 2, { sessions = { { sessionId = "", cwd = "/tmp/project" } } })
  MiniTest.expect.equality(discovered, {})
  MiniTest.expect.equality(discovery_errors, {
    { agent = "agent", message = "ACP session/list returned malformed data: session entry has malformed fields" },
  })

  assert(api:discover_sessions(nil, function()
    callbacks = callbacks + 1
  end))
  local late = processes[2]
  assert(api:dispose())
  MiniTest.expect.equality(late.closed, true)
  respond(late, 1, {
    protocolVersion = 1,
    agentCapabilities = { sessionCapabilities = { list = {} } },
  })
  MiniTest.expect.equality(callbacks, 1)

  restore_processes(original_system)
end

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
    name = "session-1",
    source = "new",
    agent = "one",
    acp_session_id = "one-acp",
    status = "ready",
    working_dir = "/tmp/one",
    current_turn = 0,
    config_options = {},
    commands = {},
    skills_policy = "native",
  })
  MiniTest.expect.equality(second:inspect(), {
    id = "session-2",
    name = "session-2",
    source = "new",
    agent = "two",
    acp_session_id = "two-acp",
    status = "ready",
    working_dir = "/tmp/two",
    current_turn = 0,
    config_options = {},
    commands = {},
    skills_policy = "native",
  })
  MiniTest.expect.equality(api:list_sessions(), { "session-1", "session-2" })
  MiniTest.expect.equality(api:get_session("session-1"), first)
  MiniTest.expect.equality(api:get_session("session-2"), second)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["snapshots the effective skill policy for each session"] = function()
  local processes, original_system = fake_processes()
  local definitions = {
    inherited = { command = "agent-inherited", args = {} },
    native = { command = "agent-native", args = {}, skills = { policy = "native" } },
  }
  local api = assert(Session.new(definitions, "off"))

  local inherited = assert(api:create_session("inherited", { cwd = "/tmp/inherited" }))
  local native = assert(api:create_session("native", { cwd = "/tmp/native" }))
  definitions.inherited.skills = { policy = "inject" }

  MiniTest.expect.equality(inherited:inspect().skills_policy, "off")
  MiniTest.expect.equality(native:inspect().skills_policy, "native")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["starts inject sessions without an external parser dependency"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    deepseek = { command = "agent", skills = { policy = "inject" } },
  }))

  local session, err = api:create_session("deepseek")

  api:dispose()
  restore_processes(original_system)
  MiniTest.expect.equality(session ~= nil, true)
  MiniTest.expect.equality(err, nil)
  MiniTest.expect.equality(#processes, 1)
end

T["new"]["tracks supported config options and replaces dependent options after a change"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local events = {}
  local session = assert(api:create_session("agent", {
    cwd = "/tmp/project",
    on_event = function(event)
      events[#events + 1] = event
    end,
  }))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, {
    sessionId = "agent-acp",
    configOptions = {
      {
        id = "model",
        name = "Model",
        category = "model",
        type = "select",
        currentValue = "small",
        options = { { value = "small", name = "Small" }, { value = "large", name = "Large" } },
      },
      { id = "brave", name = "Brave", type = "boolean", currentValue = false },
      { id = "future", name = "Future", type = "slider", currentValue = 3 },
    },
  })

  MiniTest.expect.equality(session:inspect().config_options, {
    {
      id = "model",
      name = "Model",
      category = "model",
      type = "select",
      current_value = "small",
      options = { { value = "small", name = "Small" }, { value = "large", name = "Large" } },
    },
    { id = "brave", name = "Brave", type = "boolean", current_value = false },
  })

  local changed
  assert(session:set_config_option("model", "large", function(options, err)
    changed = { options = options, error = err }
  end))
  MiniTest.expect.equality(session:inspect().status, "configuring")
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).params, {
    sessionId = "agent-acp",
    configId = "model",
    value = "large",
  })
  respond(process, 3, {
    configOptions = {
      {
        id = "model",
        name = "Model",
        category = "model",
        type = "select",
        currentValue = "large",
        options = { { value = "large", name = "Large" } },
      },
    },
  })
  MiniTest.expect.equality(changed.error, nil)
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(changed.options, session:inspect().config_options)
  changed.options[1].name = "mutated callback value"
  MiniTest.expect.equality(session:inspect().config_options[1].name, "Model")
  MiniTest.expect.equality(events[#events].type, "config_options_changed")

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = 50, size = 100 },
  })
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "config_option_update",
      configOptions = {
        {
          id = "model",
          name = "Model",
          category = "model",
          type = "select",
          currentValue = "small",
          options = { { value = "small", name = "Small" } },
        },
      },
    },
  })
  MiniTest.expect.equality(session:inspect().config_options[1].current_value, "small")
  MiniTest.expect.equality(session:inspect().context.stale, true)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["tracks context cost and reported turn usage and rejects malformed telemetry"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  session:on(function(event)
    events[#events + 1] = event
  end)

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "usage_update",
      used = 180,
      size = 200,
      cost = { amount = 1.25, currency = "USD" },
    },
  })
  MiniTest.expect.equality(session:inspect().context, {
    used = 180,
    size = 200,
    percentage = 90,
    pressure = "high",
    stale = false,
  })
  MiniTest.expect.equality(session:inspect().cost, { amount = 1.25, currency = "USD" })
  MiniTest.expect.equality(events[#events].type, "usage_updated")

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = 150, size = 200 },
  })
  MiniTest.expect.equality(session:inspect().context.pressure, "elevated")
  MiniTest.expect.equality(session:inspect().cost, { amount = 1.25, currency = "USD" })
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = 160, size = 200, cost = nvim.NIL },
  })
  MiniTest.expect.equality(session:inspect().cost, nil)
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = 190, size = 200 },
  })
  MiniTest.expect.equality(session:inspect().context.pressure, "critical")

  local request_id = assert(session:prompt("hello"))
  respond(process, request_id, {
    stopReason = "end_turn",
    usage = { total_tokens = 30, input_tokens = 20, cached_read_tokens = 7, ignored = "future" },
  })
  MiniTest.expect.equality(session:inspect().usage, {
    total_tokens = 30,
    input_tokens = 20,
    cached_read_tokens = 7,
  })

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = -1, size = 0 },
  })
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(events[#events].data.message, "malformed ACP usage_update notification")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["rejects malformed supported config options during startup"] = function()
  local processes, original_system = fake_processes()
  local ready
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session = assert(api:create_session("agent", nil, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, {
    sessionId = "agent-acp",
    configOptions = { { id = "brave", name = "Brave", type = "boolean", currentValue = "yes" } },
  })

  MiniTest.expect.equality(ready.session, nil)
  MiniTest.expect.equality(ready.error:find("malformed configOptions", 1, true) ~= nil, true)
  MiniTest.expect.equality(session:inspect().status, "error")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["rejects option changes while prompting or waiting for permission"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  assert(session:prompt("hello"))
  MiniTest.expect.equality({ session:set_config_option("model", "large") }, { nil, "session is not idle" })

  local request = {
    jsonrpc = "2.0",
    id = 9,
    method = "session/request_permission",
    params = { sessionId = "agent-acp", options = { "allow", "deny" } },
  }
  process.options.stdout(nil, assert(Protocol.encode(request)) .. "\n")
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  MiniTest.expect.equality({ session:set_config_option("model", "large") }, { nil, "session is not idle" })
  assert(session:cancel())
  MiniTest.expect.equality(session:inspect().status, "cancelling")

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

  local streamed = {}
  for _, event in ipairs(events) do
    if event.type ~= "state_changed" then
      streamed[#streamed + 1] = event
    end
  end
  MiniTest.expect.equality({ streamed[1].type, streamed[2].type, streamed[3].type, streamed[4].type }, {
    "chunk",
    "tool_call_started",
    "tool_call_finished",
    "turn_done",
  })
  MiniTest.expect.equality(streamed[1].data.content.text, "hello")
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
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).id, 9)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["publishes overlapping permission requests one at a time"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  assert(session:prompt("hello"))
  local permissions = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)

  permission_request(process, "agent-acp", 9, { "allow", "deny" })
  permission_request(process, "agent-acp", 10, { "allow", "deny" })

  MiniTest.expect.equality(#permissions, 1)
  MiniTest.expect.equality(permissions[1].data.request_id, 9)
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")

  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "allow" } }))
  MiniTest.expect.equality(#permissions, 2)
  MiniTest.expect.equality(permissions[2].data.request_id, 10)
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  MiniTest.expect.equality(
    { permissions[1].respond({ outcome = { outcome = "cancelled" } }) },
    { false, "permission request was already answered" }
  )

  assert(permissions[2].respond({ outcome = { outcome = "cancelled" } }))
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(permission_outcomes(process), {
    { id = 9, outcome = "selected" },
    { id = 10, outcome = "cancelled" },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["cancels every outstanding permission request when the turn is cancelled"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  assert(session:prompt("hello"))
  local permissions = {}
  local cancelled = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    elseif event.type == "permission_cancelled" then
      cancelled[#cancelled + 1] = event.data
    end
  end)

  permission_request(process, "agent-acp", 9, { "allow", "deny" })
  permission_request(process, "agent-acp", 10, { "allow", "deny" })
  assert(session:cancel())

  MiniTest.expect.equality(session:inspect().status, "cancelling")
  MiniTest.expect.equality(#permissions, 1)
  MiniTest.expect.equality(cancelled, { { request_ids = { 9, 10 } } })
  MiniTest.expect.equality(permission_outcomes(process), {
    { id = 9, outcome = "cancelled" },
    { id = 10, outcome = "cancelled" },
  })
  MiniTest.expect.equality(
    { permissions[1].respond({ outcome = { outcome = "selected", optionId = "allow" } }) },
    { false, "permission request was already answered" }
  )
  MiniTest.expect.equality(#permission_outcomes(process), 2)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["publishes pathless edit permission requests as generic decisions"] = function()
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
    params = {
      sessionId = "agent-acp",
      toolCall = { toolCallId = "call-1", kind = "edit", status = "pending" },
      options = {
        { optionId = "allow-once", kind = "allow_once" },
        { optionId = "reject-once", kind = "reject_once" },
      },
    },
  }
  process.options.stdout(nil, assert(Protocol.encode(request)) .. "\n")

  assert(permission ~= nil, "pathless edit permission was not published")
  MiniTest.expect.equality(permission.data.operation, { kind = "unknown" })
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow-once" } }))
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).result, {
    outcome = { outcome = "selected", optionId = "allow-once" },
  })

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

T["new"]["persists exact always choices and replays only through compatible option kinds"] = function()
  local root = nvim.fn.tempname()
  local state_path = nvim.fs.joinpath(root, "permissions.json")
  local processes, original_system = fake_processes()
  local store = assert(Permission.store(state_path))
  local api = assert(Session.new({ agent = { command = "agent", args = { "serve" } } }, nil, {
    permission_store = store,
  }))
  local session, process = start_ready_session(api, processes, "agent", nvim.fs.joinpath(root, "workspace"))
  local permission_events = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permission_events[#permission_events + 1] = event
    end
  end)
  local function request(id, options)
    process.options.stdout(nil, assert(Protocol.encode({
      jsonrpc = "2.0",
      id = id,
      method = "session/request_permission",
      params = {
        sessionId = "agent-acp",
        toolCall = { kind = "execute", rawInput = { command = { "git", "status" } } },
        options = options,
      },
    })) .. "\n")
  end

  request(9, {
    { optionId = "allow-once", kind = "allow_once" },
    { optionId = "allow-always", kind = "allow_always" },
  })
  assert(permission_events[1].respond({ outcome = { outcome = "selected", optionId = "allow-always" } }))
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).result, {
    outcome = { outcome = "selected", optionId = "allow-always" },
  })
  local remembered = assert(api:list_permissions())
  MiniTest.expect.equality(#remembered, 1)
  MiniTest.expect.equality(remembered[1].command, { "git", "status" })
  MiniTest.expect.equality(remembered[1].adapter, { command = "agent", args = { "serve" } })

  request(10, { { optionId = "allow-once", kind = "allow_once" } })
  MiniTest.expect.equality(#permission_events, 1)
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).result, {
    outcome = { outcome = "selected", optionId = "allow-once" },
  })

  request(11, { { optionId = "custom", name = "Allow", kind = "custom" } })
  MiniTest.expect.equality(#permission_events, 2)
  assert(permission_events[2].respond({ outcome = { outcome = "cancelled" } }))
  api:dispose()

  local reloaded_api = assert(Session.new({ agent = { command = "agent", args = { "serve" } } }, nil, {
    permission_store = assert(Permission.store(state_path)),
  }))
  local reloaded, reloaded_process =
    start_ready_session(reloaded_api, processes, "agent", nvim.fs.joinpath(root, "workspace"))
  local replayed_event
  reloaded:on(function(event)
    if event.type == "permission_requested" then
      replayed_event = event
    end
  end)
  reloaded_process.options.stdout(nil, assert(Protocol.encode({
    jsonrpc = "2.0",
    id = 12,
    method = "session/request_permission",
    params = {
      sessionId = "agent-acp",
      toolCall = { kind = "execute", rawInput = { command = { "git", "status", "--short" } } },
      options = { { optionId = "once", kind = "allow_once" } },
    },
  })) .. "\n")

  MiniTest.expect.equality(replayed_event, nil)
  MiniTest.expect.equality(
    assert(Protocol.decode(reloaded_process.writes[#reloaded_process.writes]:sub(1, -2))).result,
    {
      outcome = { outcome = "selected", optionId = "once" },
    }
  )
  MiniTest.expect.equality(reloaded_api:revoke_permission(remembered[1].id), true)
  MiniTest.expect.equality(assert(reloaded_api:list_permissions()), {})
  reloaded_api:dispose()
  restore_processes(original_system)
  nvim.fn.delete(root, "rf")
end

T["new"]["applies a fresh always choice to permission requests already queued"] = function()
  local root = nvim.fn.tempname()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }, nil, {
    permission_store = assert(Permission.store(nvim.fs.joinpath(root, "permissions.json"))),
  }))
  local session, process = start_ready_session(api, processes, "agent", nvim.fs.joinpath(root, "workspace"))
  local permissions = {}
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)
  local function request(id)
    process.options.stdout(nil, assert(Protocol.encode({
      jsonrpc = "2.0",
      id = id,
      method = "session/request_permission",
      params = {
        sessionId = "agent-acp",
        toolCall = { kind = "execute", rawInput = { command = { "git", "status" } } },
        options = {
          { optionId = "allow-once", kind = "allow_once" },
          { optionId = "allow-always", kind = "allow_always" },
        },
      },
    })) .. "\n")
  end

  request(9)
  request(10)
  MiniTest.expect.equality(#permissions, 1)
  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "allow-always" } }))

  MiniTest.expect.equality(#permissions, 1)
  MiniTest.expect.equality(permission_outcomes(process), {
    { id = 9, outcome = "selected" },
    { id = 10, outcome = "selected" },
  })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).result, {
    outcome = { outcome = "selected", optionId = "allow-once" },
  })
  MiniTest.expect.equality(session:inspect().status, "prompting")

  api:dispose()
  restore_processes(original_system)
  nvim.fn.delete(root, "rf")
end

T["new"]["asks again and cancels an always choice when permission state is malformed"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(root, "p") == 1)
  local state_path = nvim.fs.joinpath(root, "permissions.json")
  assert(nvim.fn.writefile({ "not json" }, state_path) == 0)
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }, nil, {
    permission_store = assert(Permission.store(state_path)),
  }))
  local session, process = start_ready_session(api, processes, "agent", root)
  local permission_event
  session:on(function(event)
    if event.type == "permission_requested" then
      permission_event = event
    end
  end)

  process.options.stdout(nil, assert(Protocol.encode({
    jsonrpc = "2.0",
    id = 9,
    method = "session/request_permission",
    params = {
      sessionId = "agent-acp",
      toolCall = { kind = "execute", rawInput = { command = { "git", "status" } } },
      options = { { optionId = "always", kind = "allow_always" } },
    },
  })) .. "\n")

  MiniTest.expect.equality(permission_event.data.permission_error, "permission state is not valid JSON")
  local sent, send_error = permission_event.respond({ outcome = { outcome = "selected", optionId = "always" } })
  MiniTest.expect.equality(sent, false)
  MiniTest.expect.equality(
    send_error,
    "permission choice was cancelled because it could not be remembered: permission state is not valid JSON"
  )
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).result, {
    outcome = { outcome = "cancelled" },
  })
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(nvim.fn.readfile(state_path), { "not json" })
  api:dispose()
  restore_processes(original_system)
  nvim.fn.delete(root, "rf")
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

T["new"]["tracks advertised commands and replaces the cache on every update"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  MiniTest.expect.equality(session:inspect().commands, {})

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "available_commands_update",
      availableCommands = {
        { name = "grill-me", description = "Stress-test an idea" },
        { name = "plan", description = "Draft an execution plan" },
      },
    },
  })
  MiniTest.expect.equality(session:inspect().commands, {
    { name = "grill-me", description = "Stress-test an idea" },
    { name = "plan", description = "Draft an execution plan" },
  })
  MiniTest.expect.equality(events[#events].type, "commands_changed")
  MiniTest.expect.equality(events[#events].data.commands, session:inspect().commands)
  MiniTest.expect.equality(events[#events].data.diagnostics, {})

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "available_commands_update",
      availableCommands = { { name = "research", description = "Research the codebase" } },
    },
  })
  MiniTest.expect.equality(session:inspect().commands, { { name = "research", description = "Research the codebase" } })

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "available_commands_update", availableCommands = {} },
  })
  MiniTest.expect.equality(session:inspect().commands, {})

  api:dispose()
  restore_processes(original_system)
end

T["new"]["skips malformed advertised commands, diagnoses them, and keeps duplicates visible"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "available_commands_update",
      availableCommands = {
        { name = "grill-me", description = "Stress-test an idea" },
        { name = "grill-me", description = "Stress-test an idea" },
        { name = "", description = "empty name" },
        { name = "no-description" },
        "not a table",
      },
    },
  })
  MiniTest.expect.equality(session:inspect().commands, {
    { name = "grill-me", description = "Stress-test an idea" },
    { name = "grill-me", description = "Stress-test an idea" },
  })
  MiniTest.expect.equality(#events[#events].data.diagnostics, 3)

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "available_commands_update", availableCommands = "not an array" },
  })
  MiniTest.expect.equality(session:inspect().commands, {})
  MiniTest.expect.equality(#events[#events].data.diagnostics, 1)
  MiniTest.expect.equality(session:inspect().status, "ready")

  api:dispose()
  restore_processes(original_system)
end

return T
