local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Protocol = require("louiselm.acp.protocol")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local api, directory, process, original_system

local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "option recording did not settle")
end

-- Response structure: codex/01a07bb4-c41e-7010-8332-d9a6ff731e2c,
-- ~/.local/state/acp-llm-adapter/proxy/sessions/<id>/log.jsonl, responses 3-5.
-- Notification structure: proxy Session 071da992-eacd-4cf3-9752-c52cf8453186,
-- same log directory, session/update config_option_update. Values are sanitized.
-- The interleavings below are adversarial scenarios, not observed peer ordering.
local function options(effort, model, enabled)
  local result = {
    {
      id = "reasoning_effort",
      name = "Reasoning effort",
      type = "select",
      category = "thought_level",
      currentValue = effort or "medium",
      options = {
        { value = "medium", name = "Medium" },
        { value = "high", name = "High" },
      },
    },
    {
      id = "model",
      name = "Model",
      type = "select",
      category = "model",
      currentValue = model or "model-a",
      options = {
        { value = "model-a", name = "A" },
        { value = "model-b", name = "B" },
      },
    },
  }
  if enabled ~= nil then
    -- Synthetic boolean extension exercises the existing supported typed contract.
    result[#result + 1] = { id = "enabled", name = "Enabled", type = "boolean", currentValue = enabled }
  end
  return result
end

local function feed(message)
  local timer = assert(nvim.uv.new_timer())
  local delivered = false
  timer:start(0, 0, function()
    timer:close()
    assert(nvim.in_fast_event())
    process.options.stdout(nil, assert(Protocol.encode(message)) .. "\n")
    delivered = true
  end)
  wait_for(function()
    return delivered
  end)
end

local function respond(id, result)
  feed(Protocol.response(id, result))
end

local function notify(value)
  feed(assert(Protocol.notification("session/update", {
    sessionId = "recording-session",
    update = { sessionUpdate = "config_option_update", configOptions = value },
  })))
end

local function flush()
  local done, err
  api:flush_recording(function(value)
    done, err = true, value
  end)
  wait_for(function()
    return done
  end)
  return err
end

local function query(sql)
  local result = original_system({ "sqlite3", "-json", directory .. "/turns.sqlite3", sql }, { text = true }):wait()
  assert(result.code == 0, result.stderr)
  return result.stdout == "" and {} or nvim.json.decode(result.stdout)
end

local function start(provider, load)
  api = assert(
    Session.new(
      { agent = { command = "option-test-agent", provider = provider or "test-service" } },
      nil,
      { usage_directory = directory }
    )
  )
  local session = load and assert(api:load_session("agent", "recording-session")) or assert(api:create_session("agent"))
  respond(1, { protocolVersion = 1, agentCapabilities = { loadSession = true } })
  if load then
    notify(options("high"))
    notify(options("medium"))
  end
  respond(2, { sessionId = "recording-session", configOptions = options("medium") })
  MiniTest.expect.equality(session:inspect().status, "ready")
  return session
end

local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      directory = nvim.fn.tempname()
      original_system = nvim.system
      local system = original_system
      rawset(nvim, "system", function(command, opts, on_exit)
        if command[1] ~= "option-test-agent" then
          return system(command, opts, on_exit)
        end
        process = { options = opts, writes = {}, closed = false }
        return {
          write = function(_, data)
            if data ~= nil then
              process.writes[#process.writes + 1] = assert(Protocol.decode(data:sub(1, -2)))
            end
          end,
          kill = function()
            process.closed = true
          end,
          is_closing = function()
            return process.closed
          end,
        }
      end)
    end,
    post_case = function()
      if api then
        assert(api:dispose())
        flush()
      end
      rawset(nvim, "system", original_system)
      nvim.fn.delete(directory, "rf")
      api, process = nil, nil
    end,
  },
})

T["no-prompt transitions preserve typed snapshots and observed order"] = function()
  start()
  notify(options("high", "model-a", false))
  notify(options("medium", "model-a", true))
  notify(options("medium", "model-a", true))
  MiniTest.expect.equality(flush(), nil)
  local rows = query("SELECT * FROM option_events ORDER BY sequence")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(rows[1].source, "notification")
  MiniTest.expect.equality(rows[1].request, nvim.NIL)
  MiniTest.expect.equality(nvim.json.decode(rows[1].previous_options).reasoning_effort, "medium")
  MiniTest.expect.equality(nvim.json.decode(rows[1].options).enabled, false)
  MiniTest.expect.equality(nvim.json.decode(rows[2].previous_options).enabled, false)
  MiniTest.expect.equality(nvim.json.decode(rows[2].options).enabled, true)
  MiniTest.expect.equality(rows[1].observer_id, rows[2].observer_id)
  MiniTest.expect.equality(rows[1].id ~= rows[2].id, true)
  MiniTest.expect.equality(#query("SELECT * FROM turns"), 0)
end

T["response links only the requested option while retaining additional changes"] = function()
  local session = start()
  local id = assert(session:set_config_option("reasoning_effort", "high"))
  respond(id, { configOptions = options("high", "model-b") })
  MiniTest.expect.equality(flush(), nil)
  local row = query("SELECT * FROM option_events")[1]
  MiniTest.expect.equality(row.source, "response")
  MiniTest.expect.equality(nvim.json.decode(row.request), { id = id, option = "reasoning_effort", value = "high" })
  MiniTest.expect.equality(nvim.json.decode(row.options).model, "model-b")
  MiniTest.expect.equality(row.turn_id, nvim.NIL)
end

T["mid-turn changes keep the starting tuple and disqualify fixed comparisons"] = function()
  local session = start()
  local id = assert(session:prompt("not stored"))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  local request = process.writes[#process.writes]
  notify(options("high"))
  notify(options("medium"))
  respond(request.id, { stopReason = "end_turn", usage = { totalTokens = 12 } })
  MiniTest.expect.equality(flush(), nil)
  local state = session:inspect()
  MiniTest.expect.equality(state.turn_identity.options.reasoning_effort, "medium")
  MiniTest.expect.equality(state.turn_options_changed, true)
  local rows = query("SELECT * FROM option_events ORDER BY sequence")
  MiniTest.expect.equality({ rows[1].turn_id, rows[2].turn_id }, { id, id })
  MiniTest.expect.equality(
    #query("SELECT * FROM turns t WHERE NOT EXISTS (SELECT 1 FROM option_events o WHERE o.turn_id=t.id)"),
    0
  )
end

T["replay establishes a baseline without collecting historical transitions"] = function()
  start(nil, true)
  notify(options("medium"))
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM option_events"), 0)
end

T["unsolicited changes during a request do not inherit its identity"] = function()
  local session = start()
  local id = assert(session:set_config_option("reasoning_effort", "high"))
  notify(options("high"))
  respond(id, { configOptions = options("high", "model-b") })
  MiniTest.expect.equality(flush(), nil)
  local rows = query("SELECT * FROM option_events ORDER BY sequence")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(rows[1].source, "notification")
  MiniTest.expect.equality(rows[1].request, nvim.NIL)
  MiniTest.expect.equality(rows[2].source, "response")
  MiniTest.expect.equality(nvim.json.decode(rows[2].request).id, id)
  MiniTest.expect.equality(nvim.json.decode(rows[2].previous_options).reasoning_effort, "high")
end

T["failed requests and unchanged advertisements create no transition"] = function()
  local session = start()
  local failed
  local id = assert(session:set_config_option("reasoning_effort", "high", function(_, err)
    failed = err
  end))
  feed({ jsonrpc = "2.0", id = id, error = { code = -32602, message = "refused" } })
  assert(failed ~= nil)
  local renamed = options("medium")
  renamed[1].name = "Different display name"
  notify(renamed)
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM option_events"), 0)
end

T["unresolved Provider keeps accepted values and a visible error until correction"] = function()
  local session = start({ option = "model", prefixes = { ["model-a"] = "test-service" } })
  local observed = {}
  session:on(function(event)
    if event.type == "recording_changed" then
      assert(not nvim.in_fast_event())
      observed[#observed + 1] = event.data
    end
  end)
  notify(options("high", "model-b"))
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(session:inspect().config_options[2].current_value, "model-b")
  MiniTest.expect.equality(session:inspect().recording_error.code, "attribution")
  MiniTest.expect.equality(observed[#observed].error.code, "attribution")
  local writes = #process.writes
  local id, err = session:prompt("unresolved")
  MiniTest.expect.equality(id, nil)
  assert(err ~= nil)
  MiniTest.expect.equality(#process.writes, writes)
  notify(options("high", "model-a"))
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(session:inspect().recording_error, nil)
  MiniTest.expect.equality(#query("SELECT * FROM option_events"), 2)
  assert(session:prompt("resolved"))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
  MiniTest.expect.equality(flush(), nil)
end

T["failed writes retain actual accepted changes and gate later prompts until retry"] = function()
  local session = start()
  MiniTest.expect.equality(flush(), nil)
  assert(nvim.uv.fs_chmod(directory, 320))
  notify(options("high"))
  local failure = flush()
  MiniTest.expect.equality(failure.code, "permissions")
  MiniTest.expect.equality(session:inspect().config_options[1].current_value, "high")
  MiniTest.expect.equality(session:inspect().status, "ready")
  local writes, blocked = #process.writes, false
  assert(session:prompt("blocked by option write", function(_, err)
    assert(err ~= nil)
    blocked = true
  end))
  wait_for(function()
    return blocked
  end)
  MiniTest.expect.equality(#process.writes, writes)
  assert(nvim.uv.fs_chmod(directory, 448))
  -- Retry the unchanged encoded event, then prove a new prompt reaches the peer.
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM option_events"), 1)
  assert(session:prompt("after recovery"))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
  MiniTest.expect.equality(flush(), nil)
end

T["Disposal observers cannot overtake the change or receive later recording events"] = function()
  local session = start()
  local recording_events = 0
  session:on(function(event)
    if event.type == "config_options_changed" then
      assert(session:dispose())
    end
    if event.type == "recording_changed" then
      recording_events = recording_events + 1
    end
  end)
  notify(options("high"))
  notify(options("medium"))
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(recording_events, 0)
  local rows = query("SELECT * FROM option_events")
  MiniTest.expect.equality(#rows, 1)
  MiniTest.expect.equality(nvim.json.decode(rows[1].options).reasoning_effort, "high")
end

T["identical retry is idempotent and conflicting metadata cannot rewrite history"] = function()
  start()
  notify(options("high"))
  MiniTest.expect.equality(flush(), nil)
  local row = query("SELECT * FROM option_events")[1]
  local record = {
    kind = "options",
    id = row.id,
    observer_id = row.observer_id,
    sequence = row.sequence,
    agent = row.agent,
    acp_session_id = row.acp_session_id,
    observed_at = row.observed_at,
    previous_options = nvim.json.decode(row.previous_options),
    options = nvim.json.decode(row.options),
    source = "notification",
  }
  local retry = assert(require("louiselm.session.recording").new(directory, function() end))
  local function append()
    local done, error
    retry:append(record, function(value)
      done, error = true, value
    end)
    wait_for(function()
      return done
    end)
    return error
  end
  MiniTest.expect.equality(append(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM option_events"), 1)
  record.options.reasoning_effort = "medium"
  MiniTest.expect.equality(append().code, "conflict")
  MiniTest.expect.equality(query("SELECT * FROM option_events")[1].options, row.options)
end

T["schema upgrade preserves prior turn facts and resumed observation streams stay distinct"] = function()
  local session = start()
  assert(session:prompt("previous turn"))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  respond(process.writes[#process.writes].id, { stopReason = "end_turn", usage = { totalTokens = 3 } })
  MiniTest.expect.equality(flush(), nil)
  local turns = query("SELECT * FROM turns")
  -- Reconstitute v1 in this disposable database: the new table is still empty.
  query("DROP TABLE option_events; PRAGMA user_version=1")
  notify(options("high"))
  MiniTest.expect.equality(flush(), nil)
  local first = query("SELECT * FROM option_events")[1]
  assert(api:dispose())
  MiniTest.expect.equality(flush(), nil)
  start(nil, true)
  notify(options("high"))
  MiniTest.expect.equality(flush(), nil)
  local rows = query("SELECT * FROM option_events ORDER BY rowid")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(rows[2].observer_id ~= first.observer_id, true)
  MiniTest.expect.equality(rows[2].sequence, 1)
  MiniTest.expect.equality(query("SELECT * FROM turns"), turns)
  MiniTest.expect.equality(query("PRAGMA user_version")[1].user_version, 2)
end

return T
