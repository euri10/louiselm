local MiniTest = require("mini.test")
local Session = require("louiselm.session")
local Protocol = require("louiselm.acp.protocol")
local Qualification = require("louiselm.routing.qualification")

---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local api, directory, process, original_system, approval
local observed_decision

local function wait_for(predicate)
  assert(nvim.wait(6000, predicate, 10), "routing submission did not settle")
end

-- Advertised option/response shape follows the sanitized Codex observations in
-- option_recording_spec.lua. Candidate values and interleavings are synthetic.
local function options(model, effort)
  return {
    {
      id = "model",
      name = "Model",
      type = "select",
      category = "model",
      currentValue = model or "large",
      options = { { value = "large", name = "Large" }, { value = "small", name = "Small" } },
    },
    {
      id = "effort",
      name = "Effort",
      type = "select",
      category = "thought_level",
      currentValue = effort or "high",
      options = { { value = "high", name = "High" }, { value = "low", name = "Low" } },
    },
  }
end

local function respond(id, result)
  local timer = assert(nvim.uv.new_timer())
  local delivered = false
  timer:start(0, 0, function()
    timer:close()
    assert(nvim.in_fast_event())
    process.options.stdout(nil, assert(Protocol.encode(Protocol.response(id, result))) .. "\n")
    delivered = true
  end)
  wait_for(function()
    return delivered
  end)
end

local function report()
  return {
    version = 1,
    id = "comparison",
    policy_revision = "policy-1",
    workload = { kind = "main", id = "implementation" },
    baseline = {
      agent = "agent",
      provider = "OpenAI",
      model = "large",
      model_option_id = "model",
      options = { model = "large", effort = "high" },
    },
    candidate = {
      agent = "agent",
      provider = "OpenAI",
      model = "small",
      model_option_id = "model",
      options = { model = "small", effort = "low" },
    },
    fixtures = {
      {
        id = "task",
        source = "selected fixture",
        digest = "sha256:" .. string.rep("a", 64),
        checks = { { id = "result", baseline = "pass", candidate = "pass" } },
        human_review_required = false,
      },
    },
  }
end

local function decide(action, selected, economics)
  local done, error_message
  approval:decide({ action = action, report = selected or report(), economics = economics }, function(result, err)
    error_message, done = err, result ~= nil or err ~= nil
  end)
  wait_for(function()
    return done
  end)
  assert(error_message == nil, error_message)
end

local function start(qualified)
  approval = assert(Qualification.new(directory .. "/qualifications.json"))
  if qualified then
    decide("approve", nil, {
      { route = "baseline", kind = "estimated", metric = "api_cost", value = 1, unit = "USD", provenance = "operator" },
      {
        route = "candidate",
        kind = "estimated",
        metric = "api_cost",
        value = 0.25,
        unit = "USD",
        provenance = "operator",
      },
    })
  end
  api = assert(Session.new({
    agent = {
      command = "routing-test-agent",
      provider = "OpenAI",
      auto = {
        model = "large",
        effort = "high",
        rules = { implementation = { model = "small", effort = "low", policy_revision = "policy-1" } },
      },
    },
  }, nil, { usage_directory = directory, qualification_path = directory .. "/qualifications.json" }))
  local owned_api = api
  MiniTest.finally(function()
    assert(owned_api:dispose())
    local done
    owned_api:flush_recording(function()
      done = true
    end)
    wait_for(function()
      return done
    end)
  end)
  local session = assert(api:create_session("agent"))
  session:on(function(event)
    if event.type == "admission_decided" then
      observed_decision = event.data
    end
  end)
  respond(1, { protocolVersion = 1 })
  respond(2, { sessionId = "routing-session", configOptions = options() })
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  return session
end

local function latest(method)
  local arrived = nvim.wait(6000, function()
    return process.writes[#process.writes].method == method
  end, 10)
  local session = assert(api:get_session(api:list_sessions()[1]))
  assert(
    arrived,
    nvim.inspect({
      waiting_for = method,
      last_method = process.writes[#process.writes].method,
      status = session:inspect().status,
      recording_error = session:inspect().recording_error,
      decision = observed_decision,
    })
  )
  return process.writes[#process.writes]
end

local function query(sql)
  local done
  api:flush_recording(function(err)
    assert(err == nil, err and err.message)
    done = true
  end)
  wait_for(function()
    return done
  end)
  local result = original_system(
    { "sqlite3", "-cmd", ".timeout 6000", "-json", directory .. "/turns.sqlite3", sql },
    { text = true }
  ):wait()
  assert(result.code == 0, result.stderr)
  return result.stdout == "" and {} or nvim.json.decode(result.stdout)
end

local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      directory = nvim.fn.tempname()
      original_system = nvim.system
      local system = original_system
      rawset(nvim, "system", function(command, opts, on_exit)
        if command[1] ~= "routing-test-agent" then
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
      rawset(nvim, "system", original_system)
      nvim.fn.delete(directory, "rf")
      api, process, approval = nil, nil, nil
    end,
  },
})

T["approved selection reaches real admission and immutable turn attribution"] = function()
  local session = start(true)
  local decision
  session:on(function(event)
    if event.type == "admission_decided" then
      assert(not nvim.in_fast_event())
      decision = event.data
    end
  end)
  local metadata = { workload = "implementation" }
  local id = assert(session:prompt("never a decision payload", nil, metadata))
  metadata.workload = "changed after invocation"
  local request = latest("session/set_config_option")
  MiniTest.expect.equality(request.params.value, "small")
  respond(request.id, { configOptions = options("small", "high") })
  request = latest("session/set_config_option")
  MiniTest.expect.equality(request.params.value, "low")
  respond(request.id, { configOptions = options("small", "low") })
  request = latest("session/prompt")
  respond(request.id, { stopReason = "end_turn" })
  local saved = nvim.json.decode(query("SELECT data FROM admission_events WHERE phase='decision'")[1].data)
  MiniTest.expect.equality(decision.reason, "qualified")
  MiniTest.expect.equality(saved.selection.baseline, { model = "large", effort = "high" })
  MiniTest.expect.equality(saved.selection.workload.id, "implementation")
  MiniTest.expect.equality(saved.selection.qualification.report_id, "comparison")
  MiniTest.expect.equality(saved.selection.economic_basis[1].kind, "estimated")
  MiniTest.expect.equality(saved.selection.economic_basis[2].route, "candidate")
  assert(not nvim.json.encode(saved):find("never a decision payload", 1, true))
  local turn = query("SELECT id,options FROM turns")[1]
  MiniTest.expect.equality(turn.id, id)
  MiniTest.expect.equality(nvim.json.decode(turn.options), { model = "small", effort = "low" })
end

T["uncertainty and missing approvals dispatch the baseline with a recorded reason"] = function()
  local session = start(false)
  assert(session:prompt("small cheap implementation prose is not metadata", nil, { workload = "implementation" }))
  local request = latest("session/prompt")
  respond(request.id, { stopReason = "end_turn" })
  local saved = nvim.json.decode(query("SELECT data FROM admission_events WHERE phase='decision'")[1].data)
  MiniTest.expect.equality(saved.requested, { model = "large", effort = "high" })
  MiniTest.expect.equality(saved.selection.fallback_reason, "unqualified")
  assert(session:prompt("unknown phase"))
  request = latest("session/prompt")
  respond(request.id, { stopReason = "end_turn" })
  local rows = query("SELECT data FROM admission_events WHERE phase='decision'")
  MiniTest.expect.equality(nvim.json.decode(rows[2].data).selection.fallback_reason, "workload_unknown")
end

T["queued replacement uses current approval and dispatches its own Skill context once"] = function()
  local original_select = nvim.ui.select
  nvim.ui.select = function(_, _, callback)
    nvim.schedule(function()
      callback(nil)
    end)
  end
  MiniTest.finally(function()
    nvim.ui.select = original_select
  end)
  local session = start(true)
  assert(session:prompt("first turn"))
  local active = latest("session/prompt")
  local Chat = require("louiselm.ui.chat")
  local chat = assert(Chat.new(api))
  MiniTest.finally(function()
    chat:dispose()
  end)
  assert(chat:attach(session))
  local view = chat.views[session:inspect().id]
  view.draft:select_skill({
    name = "implementation",
    description = "Implement",
    explicit_only = false,
    path = "/not-read/SKILL.md",
    content = "selected Skill body",
  }, false)
  view.renderer:replace_prompt(view.draft.context_prefix)
  assert(chat:submit("replaced text"))
  assert(chat:submit("latest text"))
  decide("reject")
  respond(active.id, { stopReason = "end_turn" })
  wait_for(function()
    return #process.writes >= 4 and view.pending_admission == nil
  end)
  local request = process.writes[#process.writes]
  MiniTest.expect.equality(request.method, "session/prompt")
  MiniTest.expect.equality(request.params.prompt, {
    { type = "text", text = "selected Skill body" },
    { type = "text", text = "latest text" },
  })
  MiniTest.expect.equality(observed_decision.selection.fallback_reason, "unqualified")
  MiniTest.expect.equality(observed_decision.requested, { model = "large", effort = "high" })
  MiniTest.expect.equality(view.renderer:prompt_text(), "")
  MiniTest.expect.equality(view.draft.contexts, {})
  respond(request.id, { stopReason = "end_turn" })
  local count = 0
  for _, write in ipairs(process.writes) do
    if write.method == "session/prompt" then
      count = count + 1
    end
  end
  MiniTest.expect.equality(count, 2)
end

T["revoked approval during option preparation cannot dispatch the candidate"] = function()
  local session = start(true)
  local error_message
  assert(session:prompt("retained by caller", function(_, err)
    error_message = err
  end, { workload = "implementation" }))
  local request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "high") })
  request = latest("session/set_config_option")
  decide("reject")
  respond(request.id, { configOptions = options("small", "low") })
  wait_for(function()
    return error_message ~= nil
  end)
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
  MiniTest.expect.equality(#query("SELECT id FROM turns"), 0)
  MiniTest.expect.equality(
    nvim.json.decode(query("SELECT data FROM admission_events WHERE phase='settlement'")[1].data).reason,
    "selection_changed"
  )
end

T["manual pair wins over a matching qualification"] = function()
  local session = start(true)
  assert(session:set_auto(false))
  wait_for(function()
    return session:inspect().status == "ready"
  end)
  assert(session:prompt("manual", nil, { workload = "implementation" }))
  local request = latest("session/prompt")
  respond(request.id, { stopReason = "end_turn" })
  MiniTest.expect.equality(
    nvim.json.decode(query("SELECT options FROM turns")[1].options),
    { model = "large", effort = "high" }
  )
  local rows = query("SELECT phase,data FROM admission_events ORDER BY phase")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(nvim.json.decode(rows[1].data).origin, "manual")
  MiniTest.expect.equality(nvim.json.decode(rows[1].data).selection, nil)
  MiniTest.expect.equality(nvim.json.decode(rows[2].data).result, "dispatched")
end

T["approval is rechecked after durable preparation and before the ACP write"] = function()
  local session = start(true)
  local changed, error_message
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "preparing" and not changed then
      changed = true
      decide("reject")
    end
  end)
  assert(session:prompt("not sent", function(_, err)
    error_message = err
  end, { workload = "implementation" }))
  local request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "high") })
  request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "low") })
  wait_for(function()
    return error_message ~= nil
  end)
  MiniTest.expect.equality(changed, true)
  MiniTest.expect.equality(session:inspect().status, "ready")
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
  local saved = nvim.json.decode(query("SELECT data FROM admission_events WHERE phase='settlement'")[1].data)
  MiniTest.expect.equality(saved.reason, "selection_changed")
  MiniTest.expect.equality(saved.result, "not_sent")
end

T["Agent activity arriving during final approval validation prevents the ACP write"] = function()
  local session = start(true)
  local error_message
  session:on(function(event)
    if event.type == "state_changed" and event.data.status == "prompting" then
      local message = assert(Protocol.notification("session/update", {
        sessionId = "routing-session",
        update = {
          sessionUpdate = "session_info_update",
          _meta = { ["io.github.euri10.louiselm.sessionActivity"] = { version = 1, state = "running" } },
        },
      }))
      process.options.stdout(nil, assert(Protocol.encode(message)) .. "\n")
    end
  end)
  assert(session:prompt("not sent", function(_, err)
    error_message = err
  end, { workload = "implementation" }))
  local request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "high") })
  request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "low") })
  wait_for(function()
    return error_message ~= nil
  end)
  MiniTest.expect.equality(session:inspect().status, "running")
  MiniTest.expect.equality(process.writes[#process.writes].method, "session/set_config_option")
end

local function finish_before_lookup(dispose)
  local session = start(true)
  local callbacks = 0
  local sent = #process.writes
  local id = assert(session:prompt("retained", function()
    callbacks = callbacks + 1
  end, { workload = "implementation", parent_turn_id = "parent-not-dispatched" }))
  if dispose then
    assert(session:dispose())
  else
    assert(session:cancel())
  end
  local rows = query("SELECT phase,data FROM admission_events ORDER BY phase")
  MiniTest.expect.equality(#rows, 2)
  MiniTest.expect.equality(nvim.json.decode(rows[1].data).parent_turn_id, "parent-not-dispatched")
  MiniTest.expect.equality(nvim.json.decode(rows[1].data).selection.fallback_reason, "selection_incomplete")
  MiniTest.expect.equality(nvim.json.decode(rows[2].data).result, "cancelled")
  MiniTest.expect.equality(query("SELECT turn_id FROM admission_events")[1].turn_id, id)
  MiniTest.expect.equality(#process.writes, sent)
  MiniTest.expect.equality(callbacks, dispose and 0 or 1)
end

T["cancellation before lookup preserves ancestry and rejects late selection"] = function()
  finish_before_lookup(false)
end

T["Disposal before lookup preserves ancestry and rejects late selection"] = function()
  finish_before_lookup(true)
end

T["an older matching approval remains eligible at the current global revision"] = function()
  local session = start(true)
  local other = report()
  other.id, other.workload = "other-comparison", { kind = "reader", id = "reader" }
  decide("approve", other)
  assert(session:prompt("qualified", nil, { workload = "implementation" }))
  local request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "high") })
  request = latest("session/set_config_option")
  respond(request.id, { configOptions = options("small", "low") })
  request = latest("session/prompt")
  respond(request.id, { stopReason = "end_turn" })
  local saved = nvim.json.decode(query("SELECT data FROM admission_events WHERE phase='decision'")[1].data)
  MiniTest.expect.equality(saved.selection.qualification.revision, 1)
  MiniTest.expect.equality(saved.selection.qualification.approval_revision, 2)
end

return T
