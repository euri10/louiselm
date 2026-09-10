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
    if command[1] == "sqlite3" then
      return original_system(command, options, on_exit)
    end
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
    if command[1] == "git" then
      nvim.schedule(function()
        on_exit({ code = 128, stdout = "", stderr = "not a repository" })
      end)
    end
    processes[#processes + 1] = process
    return process.handle
  end)
  return processes, original_system
end

local function restore_processes(original_system)
  rawset(nvim, "system", original_system)
end

local function fake_clock()
  local now = 0
  local timers = {}
  return function(delay_ms, callback)
    timers[#timers + 1] = { at = now + delay_ms, callback = callback }
  end, function(elapsed_ms)
    local target = now + elapsed_ms
    while true do
      table.sort(timers, function(a, b)
        return a.at < b.at
      end)
      local timer = timers[1]
      if timer == nil or timer.at > target then
        break
      end
      table.remove(timers, 1)
      now = timer.at
      timer.callback()
    end
    now = target
  end
end

-- Existing lifecycle assertions start after durable admission. The recording
-- specs separately exercise preparing, write errors and cancellation before send.
local function submit(session, ...)
  local id, err = session:prompt(...)
  if id then
    assert(nvim.wait(6000, function()
      return session:inspect().status ~= "preparing"
    end, 10))
  end
  return id, err
end

local function respond(process, id, result)
  -- ACP ids exist only after asynchronous admission; answer the wire request.
  if type(id) == "string" and id:match("^%x+$") and #id == 32 then
    for index = #process.writes, 1, -1 do
      local request = assert(Protocol.decode(process.writes[index]:sub(1, -2)))
      if request.method == "session/prompt" then
        id = request.id
        break
      end
    end
  end
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

local LIMITS_META_KEY = "io.github.euri10.louiselm"
local LIMITS_READ_METHOD = "_io.github.euri10.louiselm/account_limits/read"
local LIMITS_UPDATED_METHOD = "_io.github.euri10.louiselm/account_limits/updated"

local function limits_capabilities()
  return {
    _meta = {
      [LIMITS_META_KEY] = {
        accountLimits = {
          version = 1,
          readMethod = LIMITS_READ_METHOD,
          updatedMethod = LIMITS_UPDATED_METHOD,
        },
      },
    },
  }
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

local function start_ready_session(api, processes, name, cwd, agent_capabilities)
  local ready
  local session = assert(api:create_session(name, { cwd = cwd }, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = agent_capabilities or {} })
  -- Assert only what this helper is about. Session metadata rides in `_meta`
  -- and belongs to the tests that are actually about it.
  local params = assert(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params)
  MiniTest.expect.equality(params.cwd, cwd)
  MiniTest.expect.equality(params.mcpServers, {})
  respond(process, 2, { sessionId = name .. "-acp" })
  MiniTest.expect.equality(ready.error, nil)
  MiniTest.expect.equality(ready.session, session)
  return session, process
end

T["forensics"] = MiniTest.new_set()

T["provider"] = MiniTest.new_set()

T["provider"]["candidate usage resolves each Provider and recomputes confirmed tuples"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    agent = {
      command = "agent",
      provider = {
        { provider = "One", options = { model = "small" } },
        { provider = "Two", options = { model = "large" } },
      },
    },
  }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local function options(model, enabled)
    notification(process, "session/update", {
      sessionId = "agent-acp",
      update = {
        sessionUpdate = "config_option_update",
        configOptions = {
          {
            id = "model",
            name = "Model",
            type = "select",
            currentValue = model,
            options = {
              { value = "small", name = "Small" },
              { value = "large", name = "Large" },
              {
                value = "unknown",
                name = "Unknown",
              },
            },
          },
          { id = "enabled", name = "Enabled", type = "boolean", currentValue = enabled },
        },
      },
    })
  end
  local function measure(model, enabled, amount)
    options(model, enabled)
    local id = assert(submit(session, "measured"))
    respond(process, id, { stopReason = "end_turn", usage = { totalTokens = amount } })
  end
  local function read(id)
    local done, result, failure
    session:option_usage(id, function(rows, err)
      assert(not nvim.in_fast_event())
      done, result, failure = true, rows, err
    end)
    assert(nvim.wait(6000, function()
      return done
    end, 10))
    return result, failure
  end
  measure("small", false, 120)
  measure("large", false, 240)
  measure("large", true, 480)
  options("small", false)
  local rows = assert(read("model"))
  MiniTest.expect.equality(rows[1].provider, "One")
  MiniTest.expect.equality(rows[1].summary.tokens.total_tokens.average, 120)
  MiniTest.expect.equality(rows[2].provider, "Two")
  MiniTest.expect.equality(rows[2].summary.tokens.total_tokens.average, 240)
  MiniTest.expect.equality(rows[3].error.code, "attribution")
  options("large", true)
  rows = assert(read("model"))
  MiniTest.expect.equality(rows[1].summary.turns, 0)
  MiniTest.expect.equality(rows[2].summary.tokens.total_tokens.average, 480)
  rows = assert(read("enabled"))
  MiniTest.expect.equality(rows[1].value, true)
  MiniTest.expect.equality(rows[1].summary.tokens.total_tokens.average, 480)
  MiniTest.expect.equality(rows[2].value, false)
  MiniTest.expect.equality(rows[2].summary.tokens.total_tokens.average, 240)
  local missing, err = read("missing")
  MiniTest.expect.equality(missing, nil)
  MiniTest.expect.equality(err.code, "invalid")
  api:dispose()
  restore_processes(original_system)
end

T["provider"]["does not dispatch attribution made stale during async admission"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "service", command = "agent" } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local rejected, completion_error
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "preparing" then
      notification(process, "session/update", {
        sessionId = "agent-acp",
        update = {
          sessionUpdate = "config_option_update",
          configOptions = {
            { id = "changed", name = "Changed", type = "boolean", currentValue = true },
          },
        },
      })
    elseif event.type == "prompt_rejected" then
      rejected = event.data
    end
  end)
  assert(submit(session, "must not use old attribution", function(_, err)
    completion_error = err
  end))
  MiniTest.expect.equality(#process.writes, 2)
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(type(completion_error), "string")
  MiniTest.expect.equality(rejected.turn_id, session:inspect().turn_id)
  assert(submit(session, "use current attribution"))
  MiniTest.expect.equality(#process.writes, 3)
  api:dispose()
  restore_processes(original_system)
end

T["provider"]["prefix mappings gate dispatch and preserve the prompt-start Provider"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    agent = {
      command = "agent",
      provider = { option = "access", prefixes = { ["one/"] = "One", ["one/team/"] = "Team", ["two/"] = "Two" } },
    },
  }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local function options(value)
    notification(process, "session/update", {
      sessionId = "agent-acp",
      update = {
        sessionUpdate = "config_option_update",
        configOptions = {
          {
            id = "access",
            name = "Access",
            type = "select",
            currentValue = value,
            options = { { value = value, name = "Same display name" } },
          },
        },
      },
    })
  end
  for _, value in ipairs({ "unknown/model", "one/team/model" }) do
    options(value)
    local writes = #process.writes
    local id, err = submit(session, "must not be sent")
    MiniTest.expect.equality(id, nil)
    MiniTest.expect.equality(assert(err):find("Provider", 1, true) ~= nil, true)
    MiniTest.expect.equality(#process.writes, writes)
    MiniTest.expect.equality(session:inspect().status, "ready")
    MiniTest.expect.equality(session:inspect().current_turn, 0)
    MiniTest.expect.equality(session:inspect().turn_identity, nil)
  end
  options("one/new-model")
  local request = assert(submit(session, "resolved"))
  local identity = session:inspect().turn_identity
  MiniTest.expect.equality(identity.provider, "One")
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    options("two/new-model")
    timer:close()
  end)
  assert(nvim.wait(1000, function()
    return session:inspect().config_options[1].current_value == "two/new-model"
  end))
  MiniTest.expect.equality(session:inspect().turn_identity, identity)
  respond(process, request, { stopReason = "end_turn" })
  assert(submit(session, "next service"))
  MiniTest.expect.equality(session:inspect().turn_identity.provider, "Two")
  api:dispose()
  restore_processes(original_system)
end

T["provider"]["refuses unmatched and ambiguous routes before dispatch or turn state changes"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    agent = {
      command = "agent",
      provider = {
        { provider = "one", options = { route = "direct" } },
        { provider = "two", options = { enabled = false } },
      },
    },
  }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local function options(route, enabled)
    notification(process, "session/update", {
      sessionId = "agent-acp",
      update = {
        sessionUpdate = "config_option_update",
        configOptions = {
          {
            id = "route",
            name = "Route",
            type = "select",
            currentValue = route,
            options = { { value = "direct", name = "Direct" }, { value = "other", name = "Other" } },
          },
          { id = "enabled", name = "Enabled", type = "boolean", currentValue = enabled },
        },
      },
    })
  end
  for _, values in ipairs({ { "other", true }, { "direct", false } }) do
    options(values[1], values[2])
    local writes = #process.writes
    local id, err = submit(session, "must not be sent")
    MiniTest.expect.equality(id, nil)
    MiniTest.expect.equality(err:find("Provider", 1, true) ~= nil, true)
    MiniTest.expect.equality(#process.writes, writes)
    MiniTest.expect.equality(session:inspect().status, "ready")
    MiniTest.expect.equality(session:inspect().current_turn, 0)
    MiniTest.expect.equality(session:inspect().turn_identity, nil)
  end
  options("direct", true)
  assert(submit(session, "resolved"))
  MiniTest.expect.equality(session:inspect().turn_identity.provider, "one")
  api:dispose()
  restore_processes(original_system)
end

T["provider"]["snapshots complete typed identity before prompt events and preserves it"] = function()
  local processes, original_system = fake_processes()
  local definitions = { agent = { command = "agent", provider = "service" } }
  local api = assert(Session.new(definitions))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local function options(model, enabled)
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
            currentValue = model,
            options = { { value = "small", name = "Shared name" }, { value = "large", name = "Shared name" } },
          },
          { id = "enabled", name = "Enabled", type = "boolean", currentValue = enabled },
        },
      },
    })
  end
  options("small", false)
  local observed
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "prompting" then
      observed = session:inspect()
    end
  end)
  assert(submit(session, "first"))
  local identity =
    { agent = "agent", provider = "service", model = "small", options = { model = "small", enabled = false } }
  MiniTest.expect.equality(observed.current_turn, 1)
  MiniTest.expect.equality(observed.turn_identity, identity)
  definitions.agent.provider = "changed"
  -- Exercise an actual fast-event delivery while the prompt is active.
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    options("large", true)
    timer:close()
  end)
  assert(nvim.wait(1000, function()
    return session:inspect().config_options[1].current_value == "large"
  end))
  observed.turn_identity.options.enabled = true
  MiniTest.expect.equality(session:inspect().turn_identity, identity)
  respond(process, 3, { stopReason = "end_turn" })
  assert(submit(session, "second"))
  MiniTest.expect.equality(
    session:inspect().turn_identity,
    { agent = "agent", provider = "service", model = "large", options = { model = "large", enabled = true } }
  )
  api:dispose()
  restore_processes(original_system)
end

T["forensics"]["collects an asynchronous private record for a live Session"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(root, "p") == 1)
  local processes, original_system = fake_processes()
  local api = assert(
    Session.new(
      { agent = { provider = "test-service", command = "agent", args = {} } },
      nil,
      { forensics_directory = root }
    )
  )
  local session = start_ready_session(api, processes, "agent", "/tmp/project")
  local path, collection_error

  assert(api:collect_forensics("agent", "agent-acp", { diagnosing_session_id = "agent/diagnoser" }, function(value, err)
    path = value
    collection_error = err
  end))
  MiniTest.expect.equality(path, nil)
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return path ~= nil or collection_error ~= nil
    end, 10),
    true
  )
  MiniTest.expect.equality(collection_error, nil)
  local record = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n"))
  MiniTest.expect.equality(record.subject, { agent = "agent", acp_session_id = "agent-acp" })
  MiniTest.expect.equality(record.diagnosing_session, "agent/diagnoser")
  MiniTest.expect.equality(record.evidence_sources[1].state, "omitted")
  MiniTest.expect.equality(record.observations.capabilities.embedded_context, false)
  MiniTest.expect.equality(nvim.uv.fs_stat(path).mode % 512, 384)
  -- The hrtime() component must render as a plain integer, not scientific
  -- notation (louiselm-ysh3): a raw `tostring()` on the double once produced
  -- ids like "1787715628-2.0264597271177e+14".
  MiniTest.expect.equality(record.id:match("^%d+%-%d+$") ~= nil, true)

  api:dispose()
  restore_processes(original_system)
  nvim.fn.delete(root, "rf")
end

T["forensics"]["records embedded_context from nested promptCapabilities, not a top-level field"] = function()
  local root = nvim.fn.tempname()
  assert(nvim.fn.mkdir(root, "p") == 1)
  local processes, original_system = fake_processes()
  local api = assert(
    Session.new(
      { agent = { provider = "test-service", command = "agent", args = {} } },
      nil,
      { forensics_directory = root }
    )
  )
  local session = start_ready_session(api, processes, "agent", "/tmp/project", {
    promptCapabilities = { embeddedContext = true },
  })
  local path, collection_error

  assert(api:collect_forensics("agent", "agent-acp", nil, function(value, err)
    path = value
    collection_error = err
  end))
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return path ~= nil or collection_error ~= nil
    end, 10),
    true
  )
  MiniTest.expect.equality(collection_error, nil)
  local record = nvim.json.decode(table.concat(nvim.fn.readfile(path), "\n"))
  MiniTest.expect.equality(record.observations.capabilities.embedded_context, true)

  api:dispose()
  restore_processes(original_system)
  nvim.fn.delete(root, "rf")
end

T["new"] = MiniTest.new_set()

T["new"]["persists normalized tokens and cumulative cost evidence from fast callbacks"] = function()
  local directory = nvim.fn.tempname()
  local processes, original_system = fake_processes()
  local api =
    assert(Session.new({ agent = { provider = "service", command = "agent" } }, nil, { usage_directory = directory }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local function cost(value)
    notification(
      process,
      "session/update",
      { sessionId = "agent-acp", update = { sessionUpdate = "usage_update", used = 500, size = 1000, cost = value } }
    )
  end
  cost({ amount = 1, currency = "USD" })
  local id = assert(submit(session, "secret prompt"))
  local costs = {
    { amount = 2, currency = "USD" },
    { amount = 0.5, currency = "USD" },
    { amount = 3, currency = "EUR" },
    nvim.NIL,
    { amount = 4, currency = "USD" },
  }
  local timer = assert(nvim.uv.new_timer())
  timer:start(0, 0, function()
    assert(nvim.in_fast_event())
    for _, value in ipairs(costs) do
      cost(value)
    end
    -- CamelCase wire fields match the captured DeepSeek prompt-result fixture
    -- cited by louiselm-4tum (session-25790698-4242-41d4-af23-cfe0e512d90c).
    -- Resets/currency changes and this ordering are synthetic boundary coverage.
    respond(process, id, {
      stopReason = "end_turn",
      usage = {
        totalTokens = 30,
        inputTokens = 20,
        outputTokens = 10,
        thoughtTokens = 5,
        cachedReadTokens = 7,
        cachedWriteTokens = 0,
        ignored = "payload",
      },
    })
    timer:close()
  end)
  assert(nvim.wait(6000, function()
    return session:inspect().status == "ready"
  end, 10))
  local flushed
  api:flush_recording(function(err)
    assert(err == nil)
    assert(not nvim.in_fast_event())
    flushed = true
  end)
  assert(nvim.wait(6000, function()
    return flushed
  end, 10))
  local function rows(sql)
    local result = original_system({ "sqlite3", "-json", directory .. "/turns.sqlite3", sql }, { text = true }):wait()
    assert(result.code == 0, result.stderr)
    return nvim.json.decode(result.stdout)
  end
  MiniTest.expect.equality(
    nvim.json.decode(rows("SELECT cost_baseline FROM turns")[1].cost_baseline),
    { amount = 1, currency = "USD" }
  )
  local recorded_costs = rows("SELECT data FROM turn_events WHERE kind='cost' ORDER BY sequence")
  for index, value in ipairs(costs) do
    MiniTest.expect.equality(nvim.json.decode(recorded_costs[index].data).cost, value)
  end
  local outcome = nvim.json.decode(rows("SELECT data FROM turn_events WHERE kind='outcome'")[1].data)
  MiniTest.expect.equality(outcome.usage, {
    total_tokens = 30,
    input_tokens = 20,
    output_tokens = 10,
    thought_tokens = 5,
    cached_read_tokens = 7,
    cached_write_tokens = 0,
  })
  MiniTest.expect.equality(outcome.peer_response, true)
  assert(session:dispose())
  cost({ amount = 999, currency = "USD" })
  MiniTest.expect.equality(#rows("SELECT data FROM turn_events WHERE kind='cost'"), 5)
  assert(api:dispose())
  restore_processes(original_system)
  nvim.fn.delete(directory, "rf")
end

T["new"]["reports live Sessions across headless APIs for exit safety"] = function()
  local processes, original_system = fake_processes()
  local first_api = assert(Session.new({ claude = { provider = "test-service", command = "claude", args = {} } }))
  local second_api = assert(Session.new({ codex = { provider = "test-service", command = "codex", args = {} } }))
  local first = assert(first_api:create_session("claude"))
  respond(processes[1], 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(processes[1], 2, { sessionId = "claude-acp" })
  local second = assert(second_api:create_session("codex"))
  respond(processes[2], 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond(processes[2], 2, { sessionId = "codex-acp" })
  assert(submit(second, "working"))

  local verdict = Session.exit_verdict()

  MiniTest.expect.equality(#verdict, 2)
  MiniTest.expect.equality(verdict[1].session, first)
  MiniTest.expect.equality(verdict[1].agent, "claude")
  MiniTest.expect.equality(verdict[1].acp_session_id, "claude-acp")
  MiniTest.expect.equality(verdict[1].recoverable, false)
  MiniTest.expect.equality(verdict[1].turn_active, false)
  MiniTest.expect.equality(verdict[2].session, second)
  MiniTest.expect.equality(verdict[2].agent, "codex")
  MiniTest.expect.equality(verdict[2].recoverable, true)
  MiniTest.expect.equality(verdict[2].turn_active, true)

  assert(Session.dispose_all())
  MiniTest.expect.equality(Session.exit_verdict(), {})
  restore_processes(original_system)
end

T["identity"] = MiniTest.new_set()

T["identity"]["answers each caller separately instead of naming one current Session"] = function()
  local processes, original_system = fake_processes()
  local first_api = assert(Session.new({ claude = { provider = "test-service", command = "claude", args = {} } }))
  local second_api = assert(Session.new({ codex = { provider = "test-service", command = "codex", args = {} } }))
  assert(first_api:create_session("claude"))
  respond(processes[1], 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(processes[1], 2, { sessionId = "claude-acp" })
  assert(second_api:create_session("codex"))
  respond(processes[2], 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(processes[2], 2, { sessionId = "codex-acp" })

  -- Both answers are correct at the same instant across both headless APIs.
  -- Nothing here is "current", so no UI focus change can alias one caller onto
  -- the other's identity (louiselm-hmmc).
  MiniTest.expect.equality({ Session.identity("claude-acp") }, { "claude/claude-acp" })
  MiniTest.expect.equality({ Session.identity("codex-acp") }, { "codex/codex-acp" })

  assert(Session.dispose_all())
  MiniTest.expect.equality({ Session.identity("claude-acp") }, { nil, "no live Session has ACP session id claude-acp" })
  restore_processes(original_system)
end

T["identity"]["refuses an unmatched caller rather than falling back to the only live Session"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ claude = { provider = "test-service", command = "claude", args = {} } }))
  assert(api:create_session("claude"))
  respond(processes[1], 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(processes[1], 2, { sessionId = "claude-acp" })

  MiniTest.expect.equality(
    { Session.identity("someone-elses-acp") },
    { nil, "no live Session has ACP session id someone-elses-acp" }
  )

  assert(Session.dispose_all())
  restore_processes(original_system)
end

T["identity"]["refuses an ambiguous match instead of picking one Session"] = function()
  local processes, original_system = fake_processes()
  local first_api = assert(Session.new({ claude = { provider = "test-service", command = "claude", args = {} } }))
  local second_api = assert(Session.new({ claude = { provider = "test-service", command = "claude", args = {} } }))
  assert(first_api:create_session("claude"))
  respond(processes[1], 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond(processes[1], 2, { sessionId = "shared-acp" })
  assert(second_api:load_session("claude", "shared-acp"))
  respond(processes[2], 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond(processes[2], 2, {})

  MiniTest.expect.equality(
    { Session.identity("shared-acp") },
    { nil, "2 live Sessions have ACP session id shared-acp; ask which one is calling" }
  )

  assert(Session.dispose_all())
  restore_processes(original_system)
end

T["identity"]["rejects a caller identity that is missing or not a string"] = function()
  MiniTest.expect.equality({ Session.identity("") }, { nil, "acp_session_id must be a non-empty string" })
  ---@diagnostic disable-next-line: param-type-mismatch -- Proves the contract rejects a non-string caller id.
  MiniTest.expect.equality({ Session.identity(nil) }, { nil, "acp_session_id must be a non-empty string" })
  ---@diagnostic disable-next-line: param-type-mismatch -- Proves the contract rejects a non-string caller id.
  MiniTest.expect.equality({ Session.identity(42) }, { nil, "acp_session_id must be a non-empty string" })
end

T["new"]["reads and receives normalized Agent account limits through an advertised ACP extension"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local ready
  assert(api:create_session("agent", { cwd = "/tmp/project" }, function(session)
    ready = session
  end))
  local process = processes[1]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
  respond(process, 2, { sessionId = "agent-acp" })
  assert(ready ~= nil)

  local observed = {}
  local unsubscribe = assert(api:on_agent_limits(function(state)
    observed[#observed + 1] = state.status
  end))
  local refreshed
  assert(api:refresh_agent_limits("agent", function(state, err)
    refreshed = { state = state, error = err }
  end))
  MiniTest.expect.equality(process.writes[3]:find('"params":{}', 1, true) ~= nil, true)
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[3]:sub(1, -2))).method, LIMITS_READ_METHOD)
  respond(process, 3, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 82, windowDurationMins = 300, resetsAt = 4102444800 } },
        planType = "plus",
        credits = { balance = 7.5, unlimited = false },
      },
    },
    resetCredits = {
      availableCount = 1,
      credits = { { id = "credit-1", expiresAt = 4102448400, title = "Reset" } },
    },
  })

  MiniTest.expect.equality(refreshed.error, nil)
  MiniTest.expect.equality(refreshed.state.status, "fresh")
  MiniTest.expect.equality(observed, { "loading", "fresh" })
  MiniTest.expect.equality(refreshed.state.snapshot, {
    default_bucket_id = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { used_percent = 82, duration_mins = 300, resets_at = 4102444800 } },
        plan_type = "plus",
        credits = { balance = 7.5, unlimited = false },
      },
    },
    reset_credits = {
      available_count = 1,
      credits = { { id = "credit-1", expires_at = 4102448400, title = "Reset" } },
    },
  })

  notification(process, LIMITS_UPDATED_METHOD, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        label = "Codex",
        windows = { { usedPercent = 91, windowDurationMins = 10080, resetsAt = 4102452000 } },
        reachedType = "weekly",
      },
    },
  })
  local updated = assert(api:inspect_agent_limits("agent"))
  MiniTest.expect.equality(updated.status, "fresh")
  MiniTest.expect.equality(updated.snapshot.buckets[1].label, "Codex")
  MiniTest.expect.equality(updated.snapshot.buckets[1].windows[1].used_percent, 91)
  MiniTest.expect.equality(observed, { "loading", "fresh", "fresh" })

  unsubscribe()
  notification(process, LIMITS_UPDATED_METHOD, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 92, windowDurationMins = 10080, resetsAt = 4102452000 } },
      },
    },
  })
  MiniTest.expect.equality(observed, { "loading", "fresh", "fresh" })

  notification(process, LIMITS_UPDATED_METHOD, { buckets = {}, unlimited = true })
  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "unlimited")
  notification(process, LIMITS_UPDATED_METHOD, { buckets = {} })
  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "empty")
  notification(process, LIMITS_UPDATED_METHOD, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 10, windowDurationMins = 60, resetsAt = os.time() - 1 } },
      },
    },
  })
  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "stale")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["keeps last good account limits stale after malformed data or a refresh error"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  assert(api:create_session("agent", { cwd = "/tmp/project" }))
  local process = processes[1]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
  respond(process, 2, { sessionId = "agent-acp" })

  assert(api:refresh_agent_limits("agent", function() end))
  respond(process, 3, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 40, windowDurationMins = 60, resetsAt = 4102444800 } },
      },
    },
  })
  notification(process, LIMITS_UPDATED_METHOD, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 101, windowDurationMins = 60, resetsAt = 4102444800 } },
      },
    },
  })
  local malformed = assert(api:inspect_agent_limits("agent"))
  MiniTest.expect.equality(malformed.status, "stale")
  MiniTest.expect.equality(malformed.snapshot.buckets[1].windows[1].used_percent, 40)

  local failed
  assert(api:refresh_agent_limits("agent", function(state, err)
    failed = { state = state, error = err }
  end))
  respond_error(process, 4, { code = -32603, message = "Internal error", data = { secret = "hidden" } })
  MiniTest.expect.equality(failed.error, "ACP account limits read failed: Internal error")
  MiniTest.expect.equality(failed.state.status, "stale")
  MiniTest.expect.equality(failed.state.snapshot.buckets[1].windows[1].used_percent, 40)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["inspects unsupported and unobserved Agent limits without starting a process"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))

  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "not_observed")
  MiniTest.expect.equality(#processes, 0)

  assert(api:create_session("agent", { cwd = "/tmp/project" }))
  local process = processes[1]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "agent-acp" })
  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "unsupported")
  MiniTest.expect.equality(#processes, 1)

  local refreshed
  assert(api:refresh_agent_limits("agent", function(state, err)
    refreshed = { state = state, error = err }
  end))
  MiniTest.expect.equality(refreshed.error, nil)
  MiniTest.expect.equality(refreshed.state.status, "unsupported")
  MiniTest.expect.equality(#process.writes, 2)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["ignores account-limit results arriving after their source Session is disposed"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session = assert(api:create_session("agent", { cwd = "/tmp/project" }))
  local process = processes[1]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
  respond(process, 2, { sessionId = "agent-acp" })

  local refreshed
  assert(api:refresh_agent_limits("agent", function(state)
    refreshed = state
  end))
  session:dispose()
  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "unavailable")

  respond(process, 3, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 10, windowDurationMins = 60, resetsAt = 4102444800 } },
      },
    },
  })
  notification(process, LIMITS_UPDATED_METHOD, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 20, windowDurationMins = 60, resetsAt = 4102444800 } },
      },
    },
  })
  MiniTest.expect.equality(refreshed, nil)
  MiniTest.expect.equality(assert(api:inspect_agent_limits("agent")).status, "unavailable")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["fails account-limit refresh over to another live Session for the same Agent"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local first = assert(api:create_session("agent", { cwd = "/tmp/project" }))
  respond(processes[1], 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
  respond(processes[1], 2, { sessionId = "first-acp" })
  assert(api:create_session("agent", { cwd = "/tmp/project" }))
  respond(processes[2], 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
  respond(processes[2], 2, { sessionId = "second-acp" })

  assert(api:refresh_agent_limits("agent", function() end))
  MiniTest.expect.equality(#processes[1].writes, 3)
  MiniTest.expect.equality(#processes[2].writes, 2)
  first:dispose()

  local refreshed
  assert(api:refresh_agent_limits("agent", function(state)
    refreshed = state
  end))
  MiniTest.expect.equality(#processes[2].writes, 3)
  respond(processes[2], 3, {
    defaultBucketId = "codex",
    buckets = {
      {
        id = "codex",
        windows = { { usedPercent = 30, windowDurationMins = 60, resetsAt = 4102444800 } },
      },
    },
  })
  MiniTest.expect.equality(refreshed.status, "fresh")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["loads an existing ACP session and receives replayed history"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local ready
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session = assert(api:load_session("agent", "prior-acp", {
    cwd = "/tmp/project",
    on_event = function(event)
      events[#events + 1] = event
    end,
  }, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]

  MiniTest.expect.equality(session:inspect().acp_session_id, "prior-acp")

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))), {
    id = 2,
    jsonrpc = "2.0",
    method = "session/load",
    params = {
      sessionId = "prior-acp",
      cwd = "/tmp/project",
      mcpServers = {},
      -- A resumed Session needs the thinking default as much as a new one.
      _meta = { claudeCode = { options = { thinking = { type = "adaptive", display = "summarized" } } } },
    },
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

T["new"]["threads agent Definition.options._meta into session/new params"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    agent = {
      provider = "test-service",
      command = "agent",
      args = {},
      options = { _meta = { claudeCode = { options = { thinking = { type = "adaptive" } } } } },
    },
  }))
  assert(api:create_session("agent", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params, {
    cwd = "/tmp/project",
    mcpServers = {},
    _meta = { claudeCode = { options = { thinking = { type = "adaptive" } } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["threads agent Definition.options._meta into session/load params"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    agent = {
      provider = "test-service",
      command = "agent",
      args = {},
      options = { _meta = { claudeCode = { options = { thinking = { type = "adaptive" } } } } },
    },
  }))
  assert(api:load_session("agent", "prior-acp", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params, {
    sessionId = "prior-acp",
    cwd = "/tmp/project",
    mcpServers = {},
    _meta = { claudeCode = { options = { thinking = { type = "adaptive" } } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["invents no _meta beyond the thinking default when the definition has none"] = function()
  -- Replaces an earlier contract that sent no `_meta` at all for a definition
  -- without `options._meta`. That became obsolete when the thinking default
  -- stopped being gated (louiselm-5tuq); what still matters, and is asserted
  -- here, is that nothing else is fabricated alongside it.
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  assert(api:create_session("agent", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params, {
    cwd = "/tmp/project",
    mcpServers = {},
    _meta = { claudeCode = { options = { thinking = { type = "adaptive", display = "summarized" } } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["defaults Claude's thinking display to summarized"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    claude = { provider = "test-service", command = "agent", args = {}, transcript_layout = "claude" },
  }))
  assert(api:create_session("claude", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params._meta, {
    claudeCode = { options = { thinking = { type = "adaptive", display = "summarized" } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["defaults thinking display without any configured transcript layout"] = function()
  -- The ordinary configuration. `transcript_layout` is an optional Provenance
  -- hint, so gating the default on it shipped the feature inert for every user
  -- who left it unset, which is the documented-as-fine case (louiselm-5tuq).
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ claude = { provider = "test-service", command = "agent", args = {} } }))
  assert(api:create_session("claude", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params._meta, {
    claudeCode = { options = { thinking = { type = "adaptive", display = "summarized" } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["sends the vendor-namespaced default to a non-Claude agent too"] = function()
  -- `_meta` is ACP's extensibility namespace and the protocol requires that
  -- implementations make no assumptions about keys they do not own, so a
  -- `claudeCode` entry is precisely what a Codex Agent must ignore. Sending it
  -- unconditionally is what makes the default work without configuration.
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    codex = { provider = "test-service", command = "agent", args = {}, transcript_layout = "codex" },
  }))
  assert(api:create_session("codex", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params._meta, {
    claudeCode = { options = { thinking = { type = "adaptive", display = "summarized" } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["respects an explicit user thinking config instead of overriding it"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    claude = {
      provider = "test-service",
      command = "agent",
      args = {},
      transcript_layout = "claude",
      options = { _meta = { claudeCode = { options = { thinking = { type = "disabled" } } } } },
    },
  }))
  assert(api:create_session("claude", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[2]:sub(1, -2))).params._meta, {
    claudeCode = { options = { thinking = { type = "disabled" } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["does not mutate the shared agent definition when defaulting Claude's thinking display"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({
    claude = { provider = "test-service", command = "agent", args = {}, transcript_layout = "claude" },
  }))
  assert(api:create_session("claude", { cwd = "/tmp/project" }, function() end))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "first-acp" })

  assert(api:create_session("claude", { cwd = "/tmp/project" }, function() end))
  local second_process = processes[#processes]
  respond(second_process, 1, { protocolVersion = 1, agentCapabilities = {} })
  MiniTest.expect.equality(assert(Protocol.decode(second_process.writes[2]:sub(1, -2))).params._meta, {
    claudeCode = { options = { thinking = { type = "adaptive", display = "summarized" } } },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["replays the user's own prior messages as user_chunk events when loading a session"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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

T["new"]["emits thought_chunk events for replayed agent_thought_chunk notifications"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
      sessionUpdate = "agent_thought_chunk",
      content = { type = "text", text = "**planned** the answer" },
    },
  })
  respond(process, 2, {})

  MiniTest.expect.equality(session:inspect().status, "ready")
  -- The replay's one content-bearing event is the thought chunk; any remaining
  -- events are lifecycle notifications (e.g. the final state change).
  local thoughts = {}
  for _, event in ipairs(events) do
    if event.type == "thought_chunk" or event.type == "chunk" or event.type == "user_chunk" then
      thoughts[#thoughts + 1] = event
    end
  end
  MiniTest.expect.equality(#thoughts, 1)
  MiniTest.expect.equality(thoughts[1].type, "thought_chunk")
  MiniTest.expect.equality(thoughts[1].data.content.text, "**planned** the answer")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["closes the ACP process when loading returns a malformed result"] = function()
  local processes, original_system = fake_processes()
  local ready
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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

T["new"]["names a resource-not-found session/load failure instead of the raw ACP string"] = function()
  local processes, original_system = fake_processes()
  local ready
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session = assert(api:load_session("agent", "expired-park", nil, function(value, err)
    ready = { session = value, error = err }
  end))
  local process = processes[#processes]

  respond(process, 1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  respond_error(process, 2, {
    code = -32002,
    message = "Resource not found: expired-park",
  })

  MiniTest.expect.equality(ready.session, nil)
  MiniTest.expect.equality(
    ready.error,
    "the Agent no longer has this Park's session; it may have expired, been evicted from the Agent's"
      .. " session store, or the Agent was reinstalled or upgraded"
  )
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(process.closed, true)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["does not expose arbitrary internal ACP error details"] = function()
  local processes, original_system = fake_processes()
  local ready_error
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
    claude = { provider = "test-service", command = "claude-agent", args = {} },
    codex = { provider = "test-service", command = "codex-agent", args = {} },
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
    supported = { provider = "test-service", command = "supported-agent", args = {} },
    unsupported = { provider = "test-service", command = "unsupported-agent", args = {} },
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
    one = { provider = "test-service", command = "agent-one", args = {} },
    two = { provider = "test-service", command = "agent-two", args = {} },
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
    turn_options_changed = false,
    recording_pending = false,
    config_options = {},
    commands = {},
    skills_policy = "native",
    embedded_context = false,
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
    turn_options_changed = false,
    recording_pending = false,
    config_options = {},
    commands = {},
    skills_policy = "native",
    embedded_context = false,
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
    inherited = { provider = "test-service", command = "agent-inherited", args = {} },
    native = { provider = "test-service", command = "agent-native", args = {}, skills = { policy = "native" } },
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
    deepseek = { provider = "test-service", command = "agent", skills = { policy = "inject" } },
  }))

  local session, err = api:create_session("deepseek")

  api:dispose()
  restore_processes(original_system)
  MiniTest.expect.equality(session ~= nil, true)
  MiniTest.expect.equality(err, nil)
  MiniTest.expect.equality(#processes, 1)
end

T["new"]["records embedded context support from agent capabilities"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent" } }))
  local session = assert(api:create_session("agent"))
  local process = processes[#processes]

  respond(process, 1, {
    protocolVersion = 1,
    agentCapabilities = { promptCapabilities = { embeddedContext = true } },
  })
  respond(process, 2, { sessionId = "agent-acp" })

  MiniTest.expect.equality(session:inspect().embedded_context, true)
  api:dispose()
  restore_processes(original_system)
end

T["new"]["tracks supported config options and replaces dependent options after a change"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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

T["new"]["tracks context cost and reported turn usage and ignores malformed telemetry"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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

  local request_id = assert(submit(session, "hello"))
  respond(process, request_id, {
    stopReason = "end_turn",
    usage = { totalTokens = 30, inputTokens = 20, cachedReadTokens = 7, ignored = "future" },
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
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(session:inspect().context.stale, true)
  MiniTest.expect.equality(session:inspect().context.pressure, "critical")
  MiniTest.expect.equality(events[#events].type, "usage_updated")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["ignores malformed usage_update telemetry when no context has ever arrived"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  session:on(function(event)
    events[#events + 1] = event
  end)

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = -1, size = 0 },
  })
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(session:inspect().context, nil)
  MiniTest.expect.equality(#events, 0)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["accepts an over-full context window"] = function()
  -- Values captured from claude/ef254b92-acbf-44c3-84fd-3eedb982f46d at
  -- 2026-08-28T07:34:25.340Z, in
  -- ~/.local/state/acp-llm-adapter/proxy/sessions/ef254b92-acbf-44c3-84fd-3eedb982f46d/log.jsonl:
  -- the first mid-turn usage_update after session/load reports the agent's
  -- default 200k window before the turn result corrects it to the model's real
  -- 1M one, so `used` momentarily exceeds `size`.
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  session:on(function(event)
    events[#events + 1] = event
  end)

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "usage_update", used = 237031, size = 200000 },
  })

  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(session:inspect().context, {
    used = 237031,
    size = 200000,
    percentage = 100,
    pressure = "critical",
    stale = false,
  })
  MiniTest.expect.equality(events[#events].type, "usage_updated")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["rejects malformed reported turn usage"] = function()
  local processes, original_system = fake_processes()
  local events = {}
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  session:on(function(event)
    events[#events + 1] = event
  end)

  local request_id = assert(submit(session, "hello"))
  respond(process, request_id, {
    stopReason = "end_turn",
    usage = { totalTokens = "many" },
  })
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(events[#events].data.message, "ACP session/prompt returned malformed usage")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["rejects malformed supported config options during startup"] = function()
  local processes, original_system = fake_processes()
  local ready
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  assert(submit(session, "hello"))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  local completed
  local request_id = assert(submit(session, "hello", function(result, err)
    completed = { result = result, error = err }
  end))
  MiniTest.expect.equality(request_id, session:inspect().turn_id)
  MiniTest.expect.equality(#request_id, 32)
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
    if event.type ~= "state_changed" and event.type ~= "recording_changed" then
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

T["new"]["tracks AIR session failure revisions and clears the warning on progress"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  local request_id = assert(submit(session, "hello"))
  local function failure(revision, title, version)
    notification(process, "session/update", {
      sessionId = "agent-acp",
      update = {
        sessionUpdate = "session_info_update",
        _meta = {
          jetbrains = {
            air = {
              version = version or 1,
              sessionFailure = {
                id = "turn:error",
                revision = revision,
                category = "service",
                severity = "warning",
                title = title,
                actions = {},
              },
            },
          },
        },
      },
    })
  end

  failure(2, "Retrying Claude, attempt 2 of 10.")
  MiniTest.expect.equality(session:inspect().session_failure, {
    id = "turn:error",
    revision = 2,
    severity = "warning",
    title = "Retrying Claude, attempt 2 of 10.",
  })

  failure(1, "stale")
  MiniTest.expect.equality(session:inspect().session_failure.title, "Retrying Claude, attempt 2 of 10.")

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "session_info_update",
      _meta = {
        jetbrains = {
          air = {
            version = 1,
            sessionFailure = { id = "bad", revision = 1, severity = "warning", title = 7 },
          },
        },
      },
    },
  })
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(session:inspect().session_failure.title, "Retrying Claude, attempt 2 of 10.")

  failure(3, "unsupported", 2)
  MiniTest.expect.equality(session:inspect().session_failure.title, "Retrying Claude, attempt 2 of 10.")

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "agent_message_chunk",
      content = { type = "text", text = "recovered" },
    },
  })
  MiniTest.expect.equality(session:inspect().session_failure, nil)
  MiniTest.expect.equality(events[#events].type, "chunk")

  failure(3, "Retrying Claude, attempt 3 of 10.")
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "agent_thought_chunk",
      content = { type = "text", text = "thinking" },
    },
  })
  MiniTest.expect.equality(session:inspect().session_failure, nil)

  failure(4, "Retrying Claude, attempt 4 of 10.")
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = { sessionUpdate = "tool_call", toolCallId = "tool-1", status = "in_progress" },
  })
  MiniTest.expect.equality(session:inspect().session_failure, nil)

  failure(5, "Retrying Claude, attempt 5 of 10.")
  respond(process, request_id, { stopReason = "end_turn" })
  MiniTest.expect.equality(session:inspect().session_failure, nil)

  assert(submit(session, "again"))
  failure(6, "Retrying Claude, attempt 6 of 10.")
  assert(session:cancel())
  MiniTest.expect.equality(session:inspect().session_failure, nil)

  failure(7, "Retrying Claude, attempt 7 of 10.")
  session:dispose()
  MiniTest.expect.equality(session:inspect().session_failure, nil)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["does not emit live user message echoes as replay events"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local events = {}
  session:on(function(event)
    events[#events + 1] = event
  end)

  assert(submit(session, "hello"))
  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "user_message_chunk",
      content = { type = "text", text = "hello" },
    },
  })

  local user_chunks = {}
  for _, event in ipairs(events) do
    if event.type == "user_chunk" then
      user_chunks[#user_chunks + 1] = event
    end
  end
  MiniTest.expect.equality(#user_chunks, 0)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["does not reopen a completed turn for late permission responses"] = function()
  -- This ordering is captured from the ACP proxy trace for
  -- `opencode/ses_fad1ec8d3ffeTYD4TzGM755ney` in
  -- ~/.local/state/acp-llm-adapter/proxy/sessions/ses_fad1ec8d3ffeTYD4TzGM755ney/log.jsonl:
  -- the session/prompt response arrives after permission request 9 and before request 10.
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local permissions = {}
  local completed
  session:on(function(event)
    if event.type == "permission_requested" then
      permissions[#permissions + 1] = event
    end
  end)

  local request_id = assert(submit(session, "hello", function(result, err)
    completed = { result = result, error = err }
  end))
  local options = { "once", "always", "reject" }
  permission_request(process, "agent-acp", 7, options)
  assert(permissions[1].respond({ outcome = { outcome = "selected", optionId = "once" } }))
  permission_request(process, "agent-acp", 8, options)
  assert(permissions[2].respond({ outcome = { outcome = "cancelled" } }))
  permission_request(process, "agent-acp", 9, options)

  respond(process, request_id, { stopReason = "end_turn" })
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  MiniTest.expect.equality(completed, { result = { stopReason = "end_turn" }, error = nil })

  permission_request(process, "agent-acp", 10, options)
  assert(permissions[3].respond({ outcome = { outcome = "selected", optionId = "once" } }))
  assert(permissions[4].respond({ outcome = { outcome = "selected", optionId = "once" } }))

  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(permission_outcomes(process), {
    { id = 7, outcome = "selected" },
    { id = 8, outcome = "cancelled" },
    { id = 9, outcome = "selected" },
    { id = 10, outcome = "selected" },
  })

  api:dispose()
  restore_processes(original_system)
end

T["new"]["keeps the Session usable after a cancelled prompt reports null usage"] = function()
  -- Observed order and shapes: codex/01a074b2-a152-70b1-81eb-21ed37d04e7d,
  -- ~/.local/state/acp-llm-adapter/proxy/sessions/01a074b2-a152-70b1-81eb-21ed37d04e7d/log.jsonl:70-73.
  -- The account-limits notification between cancel and idle is unrelated and omitted.
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local timer = assert(nvim.uv.new_timer())
  MiniTest.finally(function()
    timer:stop()
    timer:close()
    api:dispose()
    restore_processes(original_system)
  end)
  local completions, turns, errors = {}, {}, {}
  session:on(function(event)
    if event.type == "turn_done" then
      turns[#turns + 1] = event.data.stopReason
    elseif event.type == "error" then
      errors[#errors + 1] = event.data.message
    end
  end)
  local request_id = assert(submit(session, "hello", function(result, err)
    completions[#completions + 1] = { result = result, error = err }
  end))
  assert(session:cancel())
  MiniTest.expect.equality(session:inspect().status, "cancelling")
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).method, "session/cancel")

  local result = {
    stopReason = "cancelled",
    usage = nvim.NIL,
    _meta = { quota = { token_count = nvim.NIL, model_usage = {} } },
  }
  local response_was_fast = false
  timer:start(0, 0, function()
    response_was_fast = nvim.in_fast_event()
    notification(process, "session/update", {
      sessionId = "agent-acp",
      update = { sessionUpdate = "session_info_update", _meta = { codex = { threadStatus = { type = "idle" } } } },
    })
    respond(process, request_id, result)
  end)
  MiniTest.expect.equality(
    nvim.wait(1000, function()
      return #completions > 0
    end, 10),
    true
  )
  MiniTest.expect.equality(response_was_fast, true)
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(session:inspect().usage, nil)
  MiniTest.expect.equality(process.closed, false)
  MiniTest.expect.equality(completions, { { result = result } })
  MiniTest.expect.equality(turns, { "cancelled" })
  MiniTest.expect.equality(errors, {})

  local next_id = assert(submit(session, "one more thing", function(next_result, err)
    completions[#completions + 1] = { result = next_result, error = err }
  end))
  respond(process, next_id, { stopReason = "end_turn", usage = { totalTokens = 5 } })
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(session:inspect().usage, { total_tokens = 5 })
  MiniTest.expect.equality(session:inspect().acp_session_id, "agent-acp")
  MiniTest.expect.equality(#completions, 2)
  MiniTest.expect.equality(turns, { "cancelled", "end_turn" })
  MiniTest.expect.equality(errors, {})
  MiniTest.expect.equality(process.closed, false)
end

T["new"]["cancels and disposes without allowing late process results"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")

  assert(submit(session, "hello"))
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

T["new"]["rejects unsupported agent requests but ignores notifications"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local _, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local writes_before = #process.writes

  local request = assert(Protocol.request(9, "session/unsupported", { sessionId = "agent-acp" }))
  process.options.stdout(nil, assert(Protocol.encode(request)) .. "\n")
  MiniTest.expect.equality(#process.writes, writes_before + 1)
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))), {
    jsonrpc = "2.0",
    id = 9,
    error = { code = -32601, message = "Method not found" },
  })

  notification(process, "session/unsupported", { sessionId = "agent-acp" })
  MiniTest.expect.equality(#process.writes, writes_before + 1)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["publishes permission requests with a response function"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  assert(submit(session, "hello"))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  assert(submit(session, "hello"))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
  local api =
    assert(Session.new({ agent = { provider = "test-service", command = "agent", args = { "serve" } } }, nil, {
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

  local reloaded_api =
    assert(Session.new({ agent = { provider = "test-service", command = "agent", args = { "serve" } } }, nil, {
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }, nil, {
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }, nil, {
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local completion
  assert(submit(session, "hello", function(result, err)
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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

T["new"]["appends buffered agent stderr to the exit error message"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local event
  session:on(function(value)
    if value.type == "error" then
      event = value
    end
  end)

  process.options.stderr(nil, "Error: opencode-ai's postinstall script was not run.\n")
  process.on_exit({ code = 1, signal = 0 })

  MiniTest.expect.equality(
    event.data.message,
    "agent process exited with code 1: Error: opencode-ai's postinstall script was not run."
  )

  api:dispose()
  restore_processes(original_system)
end

T["new"]["caps buffered agent stderr to the most recent output"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, process = start_ready_session(api, processes, "agent", "/tmp/project")
  local event
  session:on(function(value)
    if value.type == "error" then
      event = value
    end
  end)

  process.options.stderr(nil, string.rep("a", 4096))
  process.options.stderr(nil, "TAIL")
  process.on_exit({ code = 1, signal = 0 })

  local prefix = "agent process exited with code 1: "
  MiniTest.expect.equality(#event.data.message, #prefix + 4096)
  MiniTest.expect.equality(event.data.message:sub(-4), "TAIL")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["fails a session that never completes the ACP handshake after the start timeout"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local scheduled
  local ready
  local session = assert(api:create_session("agent", {
    cwd = "/tmp/project",
    schedule = function(delay_ms, callback)
      scheduled = { delay_ms = delay_ms, callback = callback }
    end,
  }, function(value, err)
    ready = { session = value, error = err }
  end))
  local event
  session:on(function(value)
    if value.type == "error" then
      event = value
    end
  end)

  MiniTest.expect.equality(session:inspect().status, "starting")
  MiniTest.expect.equality(scheduled.delay_ms, 20000)

  scheduled.callback()

  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(event.data.message, "agent did not respond within 20000ms of starting")
  MiniTest.expect.equality(ready.session, nil)
  MiniTest.expect.equality(ready.error, "agent did not respond within 20000ms of starting")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["ignores a stale start timeout after the Session becomes ready"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local scheduled
  local session = assert(api:create_session("agent", {
    cwd = "/tmp/project",
    schedule = function(delay_ms, callback)
      scheduled = { delay_ms = delay_ms, callback = callback }
    end,
  }))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "agent-acp" })
  MiniTest.expect.equality(session:inspect().status, "ready")

  scheduled.callback()

  MiniTest.expect.equality(session:inspect().status, "ready")

  api:dispose()
  restore_processes(original_system)
end

for _, responsive in ipairs({ false, true }) do
  T["new"]["resumes a silent prompt in the same Session; control responsive=" .. tostring(responsive)] = function()
    local processes, original_system = fake_processes()
    local schedule, advance = fake_clock()
    local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
    local session = assert(api:create_session("agent", { cwd = "/tmp/project", schedule = schedule }))
    local process = processes[#processes]
    respond(process, 1, { protocolVersion = 1, agentCapabilities = limits_capabilities() })
    respond(process, 2, { sessionId = "agent-acp" })

    local completion
    local completion_count = 0
    local errors = {}
    local chunks = 0
    session:on(function(event)
      if event.type == "error" then
        errors[#errors + 1] = event.data.message
      elseif event.type == "chunk" then
        chunks = chunks + 1
      end
    end)
    local request_id = assert(submit(session, "hello", function(result, err)
      completion_count = completion_count + 1
      completion = { result = result, error = err }
    end))

    -- Captured proxy sessions/01a089d3-192a-7091-8a22-8ee9d0da45fd/log.jsonl:666-674:
    -- thought -> >5m silence -> successful account-limit reads -> exit.
    -- OpenCode ses_f74032f54ffehPoLhL5Ffqhgok/log.jsonl:9176-9177 was fully silent.
    -- Payload text and subsequent recovery below are synthetic; only ordering is captured.
    notification(process, "session/update", {
      sessionId = "agent-acp",
      update = { sessionUpdate = "agent_thought_chunk", content = { type = "text", text = "working" } },
    })
    advance(360000)
    if responsive then
      local refreshed
      assert(api:refresh_agent_limits("agent", function(state, err)
        refreshed = { state = state, error = err }
      end))
      local request = assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2)))
      MiniTest.expect.equality(request.method, LIMITS_READ_METHOD)
      respond(process, request.id, { buckets = {}, unlimited = true })
      MiniTest.expect.equality(refreshed.error, nil)
      MiniTest.expect.equality(refreshed.state.status, "unlimited")
    end
    advance(360000)

    MiniTest.expect.equality(session:inspect().status, "prompting")
    MiniTest.expect.equality(process.closed, false)
    MiniTest.expect.equality(completion_count, 0)
    MiniTest.expect.equality(errors, {})

    local timer = assert(nvim.uv.new_timer())
    timer:start(0, 0, function()
      timer:close()
      notification(process, "session/update", {
        sessionId = "agent-acp",
        update = { sessionUpdate = "agent_message_chunk", content = { type = "text", text = "recovered" } },
      })
      respond(process, request_id, { stopReason = "end_turn" })
      respond(process, request_id, { stopReason = "end_turn" })
    end)
    assert(nvim.wait(1000, function()
      return completion_count > 0
    end, 10))
    advance(3600000)

    MiniTest.expect.equality(session:inspect().status, "ready")
    MiniTest.expect.equality(session:inspect().acp_session_id, "agent-acp")
    MiniTest.expect.equality(completion, { result = { stopReason = "end_turn" } })
    MiniTest.expect.equality(completion_count, 1)
    MiniTest.expect.equality(chunks, 1)
    MiniTest.expect.equality(errors, {})
    local prompts = 0
    for _, write in ipairs(process.writes) do
      if assert(Protocol.decode(write:sub(1, -2))).method == "session/prompt" then
        prompts = prompts + 1
      end
    end
    MiniTest.expect.equality(prompts, 1)

    api:dispose()
    restore_processes(original_system)
  end
end

T["new"]["keeps silent turns cancellable and ignores results after disposal"] = function()
  local processes, original_system = fake_processes()
  local schedule, advance = fake_clock()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session = assert(api:create_session("agent", { cwd = "/tmp/project", schedule = schedule }))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "agent-acp" })

  local completion
  local completion_count = 0
  local request_id = assert(submit(session, "hello", function(result, err)
    completion_count = completion_count + 1
    completion = { result = result, error = err }
  end))
  advance(720000)
  assert(session:cancel())
  MiniTest.expect.equality(session:inspect().status, "cancelling")
  MiniTest.expect.equality(assert(Protocol.decode(process.writes[#process.writes]:sub(1, -2))).method, "session/cancel")
  respond(process, request_id, { stopReason = "cancelled" })
  respond(process, request_id, { stopReason = "cancelled" })
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(completion, { result = { stopReason = "cancelled" } })
  MiniTest.expect.equality(completion_count, 1)

  request_id = assert(submit(session, "again", function()
    completion_count = completion_count + 1
  end))
  advance(720000)
  assert(session:cancel())
  assert(session:dispose())
  respond(process, request_id, { stopReason = "cancelled" })
  process.on_exit({ code = 1, signal = 0 })
  advance(720000)
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(process.closed, true)
  MiniTest.expect.equality(completion_count, 1)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["keeps a Session alive through a long human permission wait and subsequent silence"] = function()
  local processes, original_system = fake_processes()
  local schedule, advance = fake_clock()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session = assert(api:create_session("agent", {
    cwd = "/tmp/project",
    schedule = schedule,
  }))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "agent-acp" })
  local request_id = assert(submit(session, "hello"))
  local permission
  session:on(function(event)
    if event.type == "permission_requested" then
      permission = event
    end
  end)

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "tool_call_update",
      toolCallId = "chatcmpl-tool-bedebab03551b987",
      title = "read",
      status = "in_progress",
    },
  })
  permission_request(process, "agent-acp", 9, { "allow", "deny" })
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")

  -- Captured OpenCode trace ses_f9a93a9f9ffeXz1SXPE1DLPAg3 stayed here
  -- beyond two watchdog windows while its permission picker was unanswered.
  advance(720000)
  MiniTest.expect.equality(session:inspect().status, "waiting_permission")
  MiniTest.expect.equality(process.closed, false)

  assert(permission.respond({ outcome = { outcome = "selected", optionId = "allow" } }))
  MiniTest.expect.equality(session:inspect().status, "prompting")
  advance(720000)
  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(process.closed, false)

  respond(process, request_id, { stopReason = "end_turn" })
  advance(720000)
  MiniTest.expect.equality(session:inspect().status, "ready")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["keeps a prompt alive when a tool completes before prolonged silence"] = function()
  local processes, original_system = fake_processes()
  local schedule, advance = fake_clock()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session = assert(api:create_session("agent", {
    cwd = "/tmp/project",
    schedule = schedule,
  }))
  local process = processes[#processes]
  respond(process, 1, { protocolVersion = 1, agentCapabilities = {} })
  respond(process, 2, { sessionId = "agent-acp" })
  local request_id = assert(submit(session, "hello"))

  notification(process, "session/update", {
    sessionId = "agent-acp",
    update = {
      sessionUpdate = "tool_call_update",
      toolCallId = "exec-8cc6a77c-9d77-4c32-9cc6-6b543d12e1bb",
      status = "completed",
    },
  })
  advance(720000)

  MiniTest.expect.equality(session:inspect().status, "prompting")
  MiniTest.expect.equality(process.closed, false)

  respond(process, request_id, { stopReason = "end_turn" })
  advance(720000)
  MiniTest.expect.equality(session:inspect().status, "ready")

  api:dispose()
  restore_processes(original_system)
end

T["new"]["honors a custom start_timeout_ms option"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local scheduled
  assert(api:create_session("agent", {
    cwd = "/tmp/project",
    start_timeout_ms = 500,
    schedule = function(delay_ms, callback)
      scheduled = { delay_ms = delay_ms, callback = callback }
    end,
  }))

  MiniTest.expect.equality(scheduled.delay_ms, 500)

  api:dispose()
  restore_processes(original_system)
end

T["new"]["rejects a negative start_timeout_ms session option"] = function()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
  local session, err = api:create_session("agent", { start_timeout_ms = -1 })
  MiniTest.expect.equality(session, nil)
  MiniTest.expect.equality(err, "session option start_timeout_ms must be a non-negative integer")
end

T["new"]["tracks advertised commands and replaces the cache on every update"] = function()
  local processes, original_system = fake_processes()
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
  local api = assert(Session.new({ agent = { provider = "test-service", command = "agent", args = {} } }))
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
