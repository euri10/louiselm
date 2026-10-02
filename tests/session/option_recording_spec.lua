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

local function begin(provider, load, auto, initial_options)
  api = assert(
    Session.new(
      { agent = { command = "option-test-agent", provider = provider or "test-service", auto = auto } },
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
  respond(2, { sessionId = "recording-session", configOptions = initial_options or options("medium") })
  return session
end

local function start(provider, load, auto, initial_options)
  local session = begin(provider, load, auto, initial_options)
  wait_for(function()
    return session:inspect().status ~= "starting"
  end)
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

T["Auto rejection records an attempt without a prepared turn when Model removes baseline effort"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  local callback_error
  local id = assert(session:prompt("kept by caller", function(_, err)
    callback_error = err
  end, { parent_turn_id = "parent-with-no-turn" }))
  MiniTest.expect.equality(session:inspect().status, "admitting")
  wait_for(function()
    return #process.writes >= 3
  end)
  MiniTest.expect.equality(process.writes[#process.writes].params.configId, "model")
  local changed = options("medium", "model-b")
  changed[1].options = { { value = "medium", name = "Medium" } }
  respond(process.writes[#process.writes].id, { configOptions = changed })
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(callback_error, "Auto baseline effort is not supported by this Agent's current Model")
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM turns"), 0)
  local rows = query("SELECT * FROM admission_events ORDER BY phase")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(rows[1].turn_id, id)
  MiniTest.expect.equality(nvim.json.decode(rows[1].data).parent_turn_id, "parent-with-no-turn")
  MiniTest.expect.equality(nvim.json.decode(rows[2].data).result, "not_sent")
  MiniTest.expect.equality(nvim.json.decode(rows[2].data).requests.model, process.writes[#process.writes].id)
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
end

T["cancelling an in-flight Auto change holds the Session until its acknowledgement"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  local id = assert(session:prompt("kept by caller"))
  wait_for(function()
    return #process.writes >= 3
  end)
  local request = process.writes[#process.writes]
  assert(session:cancel())
  MiniTest.expect.equality(session:inspect().status, "cancelling")
  MiniTest.expect.equality({ session:prompt("racing") }, { nil, "session is not ready" })
  respond(request.id, { configOptions = options("medium", "model-b") })
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM turns"), 0)
  local row = query("SELECT * FROM admission_events WHERE phase='settlement'")[1]
  MiniTest.expect.equality(row.turn_id, id)
  MiniTest.expect.equality(nvim.json.decode(row.data).result, "cancelled")
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
end

T["operator selection pins the full pair and Auto can be selected again"] = function()
  local session = start(nil, nil, { model = "model-a", effort = "medium" })
  local request = assert(session:set_config_option("model", "model-b"))
  respond(request, { configOptions = options("high", "model-b") })
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  MiniTest.expect.equality(session:inspect().auto_mode, "manual")
  MiniTest.expect.equality(session:inspect().manual_pair, { model = "model-b", effort = "high" })
  assert(session:prompt("manual"))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/prompt")
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
  assert(session:set_auto(true))
  MiniTest.expect.equality(session:inspect().auto_mode, "auto")
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  local effort_request = assert(session:set_config_option("reasoning_effort", "medium"))
  respond(effort_request, { configOptions = options("medium", "model-b") })
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  MiniTest.expect.equality(session:inspect().auto_mode, "manual")
  MiniTest.expect.equality(session:inspect().manual_pair, { model = "model-b", effort = "medium" })
  MiniTest.expect.equality(flush(), nil)
end

T["new Auto defaults persist and resumed choices ignore changed defaults"] = function()
  local session = start(nil, nil, { model = "model-a", effort = "medium" })
  assert(session:dispose())
  MiniTest.expect.equality(flush(), nil)
  assert(api:dispose())
  session = start(nil, true, { model = "model-a", effort = "medium", default = false })
  MiniTest.expect.equality(session:inspect().auto_mode, "auto")
  assert(session:set_auto(false))
  MiniTest.expect.equality(flush(), nil)
  assert(api:dispose())
  session = start(nil, true, { model = "model-a", effort = "medium" })
  MiniTest.expect.equality(session:inspect().auto_mode, "manual")
  MiniTest.expect.equality(session:inspect().manual_pair, { model = "model-a", effort = "medium" })
end

T["preference-less resumed Sessions stay manual while new Sessions honor explicit default off"] = function()
  local session = start(nil, true, { model = "model-a", effort = "medium" })
  MiniTest.expect.equality(session:inspect().auto_mode, "manual")
  assert(api:dispose())
  session = start(nil, nil, { model = "model-a", effort = "medium", default = false })
  MiniTest.expect.equality(session:inspect().auto_mode, "manual")
end

T["resume restores the saved pair in Model-then-effort order before publishing ready"] = function()
  local baseline = { model = "model-a", effort = "medium", default = false }
  local session = start(nil, nil, baseline, options("high", "model-b"))
  assert(api:dispose())
  MiniTest.expect.equality(flush(), nil)
  session = begin(nil, true, { model = "model-a", effort = "medium" })
  MiniTest.expect.equality(session:inspect().status, "starting")
  MiniTest.expect.equality(session:inspect().auto_mode, nil)
  MiniTest.expect.equality({ session:prompt("too early") }, { nil, "session is not ready" })
  wait_for(function()
    return #process.writes == 3
  end)
  MiniTest.expect.equality(
    process.writes[3].params,
    { sessionId = "recording-session", configId = "model", value = "model-b" }
  )
  respond(process.writes[3].id, { configOptions = options("medium", "model-b") })
  MiniTest.expect.equality(process.writes[4].params.configId, "reasoning_effort")
  MiniTest.expect.equality(session:inspect().status, "starting")
  respond(process.writes[4].id, { configOptions = options("high", "model-b") })
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  MiniTest.expect.equality(session:inspect().manual_pair, { model = "model-b", effort = "high" })
  MiniTest.expect.equality(session:inspect().auto_mode, "manual")
end

T["unavailable resumed pins fail explicitly without changing the saved choice"] = function()
  start(nil, nil, { model = "model-a", effort = "medium", default = false }, options("high", "model-b"))
  assert(api:dispose())
  MiniTest.expect.equality(flush(), nil)
  local initial = options()
  initial[2].options = { { value = "model-a", name = "A" } }
  local session = begin(nil, true, { model = "model-a", effort = "medium" }, initial)
  local errors = {}
  session:on(function(event)
    if event.type == "error" then
      errors[#errors + 1] = event.data.message
    end
  end)
  wait_for(function()
    return session:inspect().status == "error"
  end)
  MiniTest.expect.equality(#process.writes, 2)
  MiniTest.expect.equality(errors[1]:find("saved pinned model is unavailable", 1, true) ~= nil, true)
  MiniTest.expect.equality(nvim.json.decode(query("SELECT preference FROM routing_preferences")[1].preference), {
    mode = "manual",
    pair = { model = "model-b", effort = "high" },
  })
end

T["internal Auto acknowledgements never replace routing authority with a manual pin"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  assert(session:prompt("automatic"))
  wait_for(function()
    return #process.writes == 3
  end)
  respond(process.writes[3].id, { configOptions = options("medium", "model-b") })
  respond(process.writes[4].id, { configOptions = options("high", "model-b") })
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  MiniTest.expect.equality(session:inspect().auto_mode, "auto")
  MiniTest.expect.equality(session:inspect().manual_pair, nil)
  MiniTest.expect.equality(
    nvim.json.decode(query("SELECT preference FROM routing_preferences")[1].preference),
    { mode = "auto" }
  )
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
end

T["saving routing authority holds admission and reports storage failure once"] = function()
  local session = start(nil, nil, { model = "model-a", effort = "medium" })
  assert(nvim.uv.fs_chmod(directory, 493))
  local calls = 0
  local callback_error ---@type string?
  assert(session:set_auto(false, function(err)
    calls, callback_error = calls + 1, err
  end))
  MiniTest.expect.equality(session:inspect().status, "configuring")
  MiniTest.expect.equality({ session:prompt("not saved") }, { nil, "session is not ready" })
  wait_for(function()
    return calls > 0
  end)
  MiniTest.expect.equality(calls, 1)
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(assert(callback_error):find("owned private directory", 1, true) ~= nil, true)
  MiniTest.expect.equality(#process.writes, 2)
  assert(nvim.uv.fs_chmod(directory, 448))
end

T["corrupt routing state refuses resume and Disposal suppresses queued preference work"] = function()
  start(nil, nil, { model = "model-a", effort = "medium" })
  assert(api:dispose())
  MiniTest.expect.equality(flush(), nil)
  query("UPDATE routing_preferences SET preference='invalid'")
  local session = begin(nil, true, { model = "model-a", effort = "medium" })
  wait_for(function()
    return session:inspect().status == "error"
  end)
  MiniTest.expect.equality(process.closed, true)
  assert(api:dispose())
  session = begin(nil, true, { model = "model-a", effort = "medium" })
  assert(session:dispose())
  local settled = false
  nvim.schedule(function()
    settled = true
  end)
  wait_for(function()
    return settled
  end)
  MiniTest.expect.equality(session:inspect().status, "disposed")
end

T["two live Sessions retain independent routing choices in one registry"] = function()
  local first = start(nil, nil, { model = "model-a", effort = "medium" })
  local second = assert(api:create_session("agent"))
  respond(1, { protocolVersion = 1, agentCapabilities = {} })
  respond(2, { sessionId = "second-session", configOptions = options() })
  wait_for(function()
    return second:inspect().status == "ready"
  end)
  assert(first:set_auto(false))
  wait_for(function()
    return first:inspect().status == "ready"
  end)
  MiniTest.expect.equality(first:inspect().auto_mode, "manual")
  MiniTest.expect.equality(second:inspect().auto_mode, "auto")
  local rows = query("SELECT acp_session_id,preference FROM routing_preferences ORDER BY acp_session_id")
  MiniTest.expect.equality(rows[1].acp_session_id, "recording-session")
  MiniTest.expect.equality(nvim.json.decode(rows[1].preference).mode, "manual")
  MiniTest.expect.equality(rows[2].acp_session_id, "second-session")
  MiniTest.expect.equality(nvim.json.decode(rows[2].preference).mode, "auto")
end

T["missing effort and unconfirmed Model replies cannot restore a pin"] = function()
  start(nil, nil, { model = "model-a", effort = "medium", default = false }, options("high", "model-b"))
  assert(api:dispose())
  MiniTest.expect.equality(flush(), nil)
  for _, unconfirmed in ipairs({ false, true }) do
    local session = begin(nil, true, { model = "model-a", effort = "medium" })
    local errors = {}
    session:on(function(event)
      if event.type == "error" then
        errors[#errors + 1] = event.data.message
      end
    end)
    wait_for(function()
      return #process.writes == 3
    end)
    local response = options("medium", unconfirmed and "model-a" or "model-b")
    response[1].options = { { value = "medium", name = "Medium" } }
    respond(process.writes[3].id, { configOptions = response })
    wait_for(function()
      return session:inspect().status == "error"
    end)
    MiniTest.expect.equality(
      errors[1]:find(unconfirmed and "did not confirm requested model" or "thought_level is unavailable", 1, true)
        ~= nil,
      true
    )
    MiniTest.expect.equality(#process.writes, 3)
    assert(api:dispose())
  end
end

T["Disposal during the configuring event cannot revive a Session or send an option request"] = function()
  local session = start(nil, nil, { model = "model-a", effort = "medium" })
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "configuring" then
      assert(session:dispose())
    end
  end)
  MiniTest.expect.equality(
    { session:set_config_option("model", "model-b") },
    { nil, "session is no longer configuring" }
  )
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(api:get_session(session:inspect().id), nil)
  MiniTest.expect.equality(#process.writes, 2)
end

T["Auto without an effort option admits a Model-only baseline"] = function()
  local initial = options("medium", "model-a")
  table.remove(initial, 1)
  local session = start(nil, nil, { model = "model-a" }, initial)
  assert(session:prompt("model only"))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/prompt")
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
  MiniTest.expect.equality(flush(), nil)
end

T["successful Auto dispatch shares one ID with durable admission and exact turn options"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  local id = assert(session:prompt("not recorded as metadata"))
  wait_for(function()
    return #process.writes >= 3
  end)
  local model_request = process.writes[#process.writes]
  respond(model_request.id, { configOptions = options("medium", "model-b") })
  local effort_request = process.writes[#process.writes]
  respond(effort_request.id, { configOptions = options("high", "model-b") })
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
  MiniTest.expect.equality(flush(), nil)
  local turn = query("SELECT * FROM turns")[1]
  MiniTest.expect.equality(turn.id, id)
  MiniTest.expect.equality(nvim.json.decode(turn.options), { model = "model-b", reasoning_effort = "high" })
  local settlement = nvim.json.decode(query("SELECT * FROM admission_events WHERE phase='settlement'")[1].data)
  MiniTest.expect.equality(settlement.result, "dispatched")
  MiniTest.expect.equality(settlement.options, nvim.json.decode(turn.options))
  MiniTest.expect.equality(settlement.requests, { model = model_request.id, reasoning_effort = effort_request.id })
end

T["helper correlation does not require Auto, a Model option, or an existing parent turn"] = function()
  local session = start(nil, nil, nil, {})
  local id = assert(session:prompt("helper prompt", nil, { parent_turn_id = "parent-not-dispatched" }))
  wait_for(function()
    return session:inspect().status == "prompting"
  end)
  respond(process.writes[#process.writes].id, { stopReason = "end_turn" })
  MiniTest.expect.equality(flush(), nil)
  local rows = query("SELECT * FROM admission_events ORDER BY phase")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(rows[1].turn_id, id)
  local decision = nvim.json.decode(rows[1].data)
  MiniTest.expect.equality(decision.origin, "helper")
  MiniTest.expect.equality(decision.parent_turn_id, "parent-not-dispatched")
  MiniTest.expect.equality(decision.requested, {})
  MiniTest.expect.equality(query("SELECT id FROM turns")[1].id, id)
end

T["Auto keeps an unsent attempt recoverable when the recording barrier fails"] = function()
  local session = start(nil, nil, { model = "model-a", effort = "medium" })
  MiniTest.expect.equality(flush(), nil)
  assert(nvim.uv.fs_chmod(directory, 320))
  MiniTest.finally(function()
    assert(nvim.uv.fs_chmod(directory, 448))
  end)
  local callback_error
  local sent = #process.writes
  local id = assert(session:prompt("caller retains this text", function(_, err)
    callback_error = err
  end))
  wait_for(function()
    return callback_error ~= nil
  end)
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(
    callback_error,
    "turn recording needs an owned private directory (0700) and regular database (0600)"
  )
  MiniTest.expect.equality(#process.writes, sent)
  assert(nvim.uv.fs_chmod(directory, 448))
  MiniTest.expect.equality(flush(), nil)
  local row = query("SELECT * FROM admission_events WHERE phase='settlement'")[1]
  MiniTest.expect.equality(row.turn_id, id)
  MiniTest.expect.equality(nvim.json.decode(row.data).reason, "recording_failed")
end

T["unknown Auto configuration response never releases admission as safe"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  local attempts = 0
  assert(session:prompt("caller retains this text", function(_, err)
    attempts = attempts + 1
    assert(err ~= nil)
  end))
  wait_for(function()
    return #process.writes >= 3
  end)
  local request = process.writes[#process.writes]
  feed({ jsonrpc = "2.0", id = request.id, error = { code = -32603, message = "uncertain" } })
  MiniTest.expect.equality(session:inspect().status, "error")
  MiniTest.expect.equality(attempts, 1)
  MiniTest.expect.equality(#process.writes, 3)
  MiniTest.expect.equality({ session:prompt("racing") }, { nil, "session is not ready" })
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(
    nvim.json.decode(query("SELECT * FROM admission_events WHERE phase='settlement'")[1].data).result,
    "uncertain"
  )
end

T["Auto refuses an effort acknowledgement that silently changes Model"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  assert(session:prompt("kept by caller"))
  wait_for(function()
    return #process.writes >= 3
  end)
  respond(process.writes[#process.writes].id, { configOptions = options("medium", "model-b") })
  local effort_request = process.writes[#process.writes]
  MiniTest.expect.equality(effort_request.params.configId, "reasoning_effort")
  respond(effort_request.id, { configOptions = options("high", "model-a") })
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(#query("SELECT * FROM turns"), 0)
  MiniTest.expect.equality(
    nvim.json.decode(query("SELECT * FROM admission_events WHERE phase='settlement'")[1].data).reason,
    "configuration_mismatch"
  )
end

T["late Auto acknowledgement after Disposal cannot dispatch"] = function()
  local session = start(nil, nil, { model = "model-b", effort = "high" })
  assert(session:prompt("kept by caller"))
  wait_for(function()
    return #process.writes >= 3
  end)
  local request = process.writes[#process.writes]
  assert(session:dispose())
  respond(request.id, { configOptions = options("medium", "model-b") })
  MiniTest.expect.equality(session:inspect().status, "disposed")
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
  MiniTest.expect.equality(flush(), nil)
  MiniTest.expect.equality(
    nvim.json.decode(query("SELECT * FROM admission_events WHERE phase='settlement'")[1].data).reason,
    "disposed"
  )
end

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
  MiniTest.expect.equality(query("PRAGMA user_version")[1].user_version, 4)
end

return T
