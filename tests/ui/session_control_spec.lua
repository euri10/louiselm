local MiniTest = require("mini.test")
local Control = require("louiselm.ui.session_control")
local T = MiniTest.new_set()
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

local function wait_for(predicate)
  assert(nvim.wait(1000, predicate, 1), "control did not settle")
end

-- Doubles retain the asynchronous production signatures. Readiness is delivered
-- from a real fast callback, so attaching a child must cross the scheduling seam.
local function fixture(skills_policy, skill_catalog)
  local children, attached = {}, {}
  local api = {}
  function api:create_session(agent, options, ready)
    local child = {
      state = {
        id = "child-" .. (#children + 1),
        agent = agent,
        status = "starting",
        working_dir = options.cwd,
        current_turn = 0,
        config_options = {},
        skills_policy = skills_policy or "native",
      },
      changes = {},
      prompts = {},
      listeners = {},
      ready = ready,
    }
    function child:inspect()
      return nvim.deepcopy(self.state)
    end
    function child:on(callback)
      self.listeners[callback] = true
      return function()
        self.listeners[callback] = nil
      end
    end
    function child:emit(kind, data)
      for callback in pairs(self.listeners) do
        callback({ type = kind, session_id = self.state.id, data = data })
      end
    end
    function child:set_config_option(id, value, callback)
      self.state.status = "configuring"
      self.changes[#self.changes + 1] = { id = id, value = value, callback = callback }
      return #self.changes
    end
    function child:prompt(prompt, callback, metadata)
      self.prompts[#self.prompts + 1] = prompt
      self.complete, self.metadata = callback, metadata
      self.state.current_turn = self.state.current_turn + 1
      self.state.turn_id = self.state.id .. "-turn"
      self.state.status = "admitting"
      return self.state.turn_id
    end
    function child:cancel()
      self.cancelled = true
      if self.state.status == "admitting" then
        -- Session:cancel (lifecycle.lua) invokes the completion failure before
        -- cancelled settlement when admission has not dispatched a turn.
        local message = "Admission cancelled; prompt was not sent"
        self:emit("prompt_rejected", { turn_id = self.state.turn_id, message = message })
        self.complete(nil, message)
        self:emit("admission_settled", { turn_id = self.state.turn_id, result = "cancelled" })
        self.state.status = "ready"
        self:emit("state_changed", { status = "ready", previous_status = "admitting" })
        return true
      end
      self.state.status = "cancelling"
      return true
    end
    function child:dispose(reason)
      self.state.status = "disposed"
      self.state.disposal_reason = reason
      self.disposed = true
      self:emit("state_changed", { status = "disposed" })
      return true
    end
    children[#children + 1] = child
    return child
  end
  local control = Control.new(api, function(session)
    assert(not nvim.in_fast_event(), "UI attachment in fast event")
    attached[#attached + 1] = session
    return 100 + #attached
  end, skill_catalog)
  MiniTest.finally(function()
    control:dispose()
  end)
  local function ready(index, options)
    local child = children[index]
    local timer = assert(nvim.uv.new_timer())
    timer:start(1, 0, function()
      timer:close()
      child.state.status = "ready"
      child.state.acp_session_id = "acp-" .. index
      child.state.config_options = options or {}
      child.ready(child)
    end)
  end
  return control, children, attached, ready
end

local function request(id)
  return { action = "start", id = id, agent = "mock", cwd = "/tmp", prompt = "task " .. id }
end

local function options(model)
  return {
    { id = "model", type = "select", current_value = model, options = { { value = "a" }, { value = "b" } } },
    { id = "effort", type = "select", current_value = "low", options = { { value = "low" }, { value = "high" } } },
  }
end

local function acknowledge(child, index, replacement)
  nvim.schedule(function()
    child.state.status = "ready"
    child.state.config_options = replacement
    child.changes[index].callback(replacement)
  end)
end

T["start is idempotent across async creation and preserves explicit assignments"] = function()
  local control, children, attached, ready = fixture()
  local first = request("first")
  first.options = { { id = "model", value = "b" }, { id = "effort", value = "high" } }
  first.parent_turn_id = "parent-turn"
  first.context = { { name = "brief", text = "explicit context" } }
  assert(control:request(first))
  assert(control:request(first))
  local second = request("second")
  second.cwd = "/var/tmp"
  assert(control:request(second))
  wait_for(function()
    return #children == 2
  end)
  ready(2)
  ready(1, options("a"))
  wait_for(function()
    return #children[1].changes == 1 and #children[2].prompts == 1
  end)
  MiniTest.expect.equality(#attached, 2)
  MiniTest.expect.equality(children[2].state.working_dir, "/var/tmp")
  MiniTest.expect.equality(children[2].prompts, { "task second" })
  MiniTest.expect.equality(#children[1].prompts, 0)
  acknowledge(children[1], 1, options("b"))
  wait_for(function()
    return #children[1].changes == 2
  end)
  local replacement = options("b")
  replacement[2].current_value = "high"
  acknowledge(children[1], 2, replacement)
  wait_for(function()
    return #children[1].prompts == 1
  end)
  MiniTest.expect.equality(children[1].metadata.parent_turn_id, "parent-turn")
  assert(nvim.inspect(children[1].prompts[1]):find("explicit context", 1, true))
  assert(control:request(first))
  first.prompt = "different assignment"
  local value, err = control:request(first)
  MiniTest.expect.equality(value, nil)
  assert(assert(err):find("different", 1, true))
  MiniTest.expect.equality(#children, 2)
end

T["contradictory option ACK refuses dispatch and retains failed operation"] = function()
  local control, children, _, ready = fixture()
  local req = request("bad-ack")
  req.options = { { id = "model", value = "b" } }
  assert(control:request(req))
  wait_for(function()
    return #children == 1
  end)
  ready(1, options("a"))
  wait_for(function()
    return #children[1].changes == 1
  end)
  acknowledge(children[1], 1, options("a"))
  wait_for(function()
    return assert(control:request({ action = "inspect", id = req.id })).phase == "failed"
  end)
  local state = assert(control:request({ action = "inspect", id = req.id }))
  assert(assert(state.error):find("confirm", 1, true))
  MiniTest.expect.equality(#children[1].prompts, 0)
  assert(control:request(req))
  MiniTest.expect.equality(#children, 1)
end

T["cancel during configuration prevents a late ACK from sending"] = function()
  local control, children, _, ready = fixture()
  local req = request("cancel-me")
  req.options = { { id = "model", value = "b" } }
  assert(control:request(req))
  wait_for(function()
    return #children == 1
  end)
  ready(1, options("a"))
  wait_for(function()
    return #children[1].changes == 1
  end)
  local state = assert(control:request({ action = "cancel", id = req.id }))
  MiniTest.expect.equality(state.phase, "cancelled")
  MiniTest.expect.equality(children[1].disposed, true)
  acknowledge(children[1], 1, options("b"))
  nvim.wait(20, function()
    return false
  end)
  MiniTest.expect.equality(#children[1].prompts, 0)
end

T["a dependent choice rejected after a Model change cannot dispatch"] = function()
  local control, children, _, ready = fixture()
  local req = request("dependent-choice")
  req.options = { { id = "model", value = "b" }, { id = "effort", value = "high" } }
  assert(control:request(req))
  wait_for(function()
    return #children == 1
  end)
  ready(1, options("a"))
  wait_for(function()
    return #children[1].changes == 1
  end)
  local replacement = options("b")
  replacement[2].options = { { value = "low" } }
  acknowledge(children[1], 1, replacement)
  wait_for(function()
    return #children[1].changes == 2
  end)
  children[1].changes[2].callback(nil, "effort is no longer advertised for this Model")
  wait_for(function()
    return assert(control:request({ action = "inspect", id = req.id })).phase == "failed"
  end)
  MiniTest.expect.equality(#children[1].prompts, 0)
  assert(control:request(req))
  MiniTest.expect.equality(#children, 1)
end

T["cancel during admission reports cancellation despite the completion failure callback"] = function()
  local control, children, _, ready = fixture()
  assert(control:request(request("admission-cancel")))
  wait_for(function()
    return #children == 1
  end)
  ready(1)
  wait_for(function()
    return #children[1].prompts == 1
  end)
  assert(control:request({ action = "cancel", id = "admission-cancel" }))
  wait_for(function()
    local state = assert(control:request({ action = "inspect", id = "admission-cancel" }))
    return state.phase == "failed" or state.phase == "cancelled"
  end)
  local state = assert(control:request({ action = "inspect", id = "admission-cancel" }))
  MiniTest.expect.equality(state.phase, "cancelled")
  MiniTest.expect.equality(state.admission, "cancelled")
  MiniTest.expect.equality(state.error, nil)
  MiniTest.expect.equality(#children[1].prompts, 1)
  MiniTest.expect.equality(next(children[1].listeners), nil)
end

T["an observed completion wins over cancellation before its scheduled delivery"] = function()
  local control, children, _, ready = fixture()
  assert(control:request(request("complete-before-cancel")))
  wait_for(function()
    return #children == 1
  end)
  ready(1)
  wait_for(function()
    return #children[1].prompts == 1
  end)
  local child = children[1]
  child:emit("admission_settled", { turn_id = child.state.turn_id, result = "dispatched" })
  child:emit("chunk", { content = { type = "text", text = "completed result" } })
  child.state.status = "ready"
  child.complete({ stopReason = "end_turn" })
  assert(control:request({ action = "cancel", id = "complete-before-cancel" }))
  wait_for(function()
    local phase = assert(control:request({ action = "inspect", id = "complete-before-cancel" })).phase
    return phase == "completed" or phase == "cancelled"
  end)
  local state = assert(control:request({ action = "inspect", id = "complete-before-cancel", result = true }))
  MiniTest.expect.equality(state.phase, "completed")
  MiniTest.expect.equality(state.result, { text = "completed result", truncated = false })
  MiniTest.expect.equality(child.disposed, nil)
  MiniTest.expect.equality(child.cancelled, nil)
end

T["bounded result truncation preserves complete UTF-8 characters"] = function()
  local control, children, _, ready = fixture()
  assert(control:request(request("utf8-result")))
  wait_for(function()
    return #children == 1
  end)
  ready(1)
  wait_for(function()
    return #children[1].prompts == 1
  end)
  local tail = string.rep("x", 8191)
  children[1]:emit("chunk", { content = { type = "text", text = "€" .. tail } })
  children[1].state.status = "ready"
  children[1].complete({ stopReason = "end_turn" })
  wait_for(function()
    return assert(control:request({ action = "inspect", id = "utf8-result" })).phase == "completed"
  end)
  local state = assert(control:request({ action = "inspect", id = "utf8-result", result = true }))
  MiniTest.expect.equality(state.result, { text = tail, truncated = true })
end

T["bounded failure truncation preserves complete UTF-8 characters"] = function()
  local control, children, _, ready = fixture()
  assert(control:request(request("utf8-failure")))
  wait_for(function()
    return #children == 1
  end)
  ready(1)
  wait_for(function()
    return #children[1].prompts == 1
  end)
  local prefix = string.rep("x", 1023)
  children[1].complete(nil, prefix .. "é")
  wait_for(function()
    return assert(control:request({ action = "inspect", id = "utf8-failure" })).phase == "failed"
  end)
  local state = assert(control:request({ action = "inspect", id = "utf8-failure" }))
  MiniTest.expect.equality(state.error, prefix)
end

T["inject policy includes its configured catalog and native policy does not"] = function()
  for _, policy in ipairs({ "inject", "native" }) do
    local control, children, _, ready = fixture(policy, "configured skill catalog")
    local req = request("catalog-" .. policy)
    req.context = { { name = "brief", text = "explicit task context" } }
    assert(control:request(req))
    wait_for(function()
      return #children == 1
    end)
    ready(1)
    wait_for(function()
      return #children[1].prompts == 1
    end)
    local sent = nvim.inspect(children[1].prompts[1])
    MiniTest.expect.equality(sent:find("configured skill catalog", 1, true) ~= nil, policy == "inject")
    assert(sent:find("explicit task context", 1, true))
  end
end

T["status omits output, explicit result is bounded, completion releases observation"] = function()
  local control, children, _, ready = fixture()
  assert(control:request(request("result")))
  wait_for(function()
    return #children == 1
  end)
  ready(1)
  local child = children[1]
  wait_for(function()
    return #child.prompts == 1
  end)
  child.state.status = "waiting_permission"
  child:emit("admission_settled", { turn_id = child.state.turn_id, result = "dispatched" })
  child:emit("permission_requested", {})
  wait_for(function()
    return assert(control:request({ action = "inspect", id = "result" })).status == "waiting_permission"
  end)
  child:emit("chunk", { content = { type = "text", text = string.rep("x", 10000) .. "final answer" } })
  nvim.schedule(function()
    child.state.status = "ready"
    child.complete({ stopReason = "end_turn" })
  end)
  wait_for(function()
    return assert(control:request({ action = "inspect", id = "result" })).phase == "completed"
  end)
  local state = assert(control:request({ action = "inspect", id = "result" }))
  MiniTest.expect.equality(state.result, nil)
  MiniTest.expect.equality(state.admission, "dispatched")
  MiniTest.expect.equality(next(child.listeners), nil)
  state = assert(control:request({ action = "inspect", id = "result", result = true }))
  assert(#state.result.text <= 8192)
  assert(state.result.text:find("final answer", 1, true))
  MiniTest.expect.equality(state.result.truncated, true)
  MiniTest.expect.equality(child.disposed, nil)
end

T["invalid requests do not create children and missing identity is an error"] = function()
  local control, children = fixture()
  local req = request("invalid")
  req.extra = true
  local value, err = control:request(req)
  MiniTest.expect.equality(value, nil)
  assert(assert(err):find("extra", 1, true))
  req.extra = nil
  req.options = { { id = "model", value = "a" }, { id = "model", value = "b" } }
  value, err = control:request(req)
  MiniTest.expect.equality(value, nil)
  assert(assert(err):find("duplicate", 1, true))
  value, err = control:request({ action = "inspect", id = "missing" })
  MiniTest.expect.equality(value, nil)
  assert(assert(err):find("unknown", 1, true))
  MiniTest.expect.equality(#children, 0)
end

T["sparse Lua arrays cannot silently drop options or context"] = function()
  local control, children = fixture()
  for _, field in ipairs({ "options", "context" }) do
    local req = request("sparse-" .. field)
    local item = field == "options" and { id = "model", value = "b" } or { name = "brief", text = "context" }
    local entries = { item, item }
    entries[1] = nil
    entries[5] = item
    -- Lua's undefined length can equal the key count even with missing index 1.
    MiniTest.expect.equality(#entries, 2)
    req[field] = entries
    local value, err = control:request(req)
    MiniTest.expect.equality(value, nil)
    assert(assert(err):find("dense", 1, true))
  end
  MiniTest.expect.equality(#children, 0)
end

T["startup deadline disposes its child and ignores late readiness"] = function()
  local control, children = fixture()
  local req = request("startup-deadline")
  req.timeout_ms = 20
  assert(control:request(req))
  wait_for(function()
    return #children == 1
  end)
  wait_for(function()
    return assert(control:request({ action = "inspect", id = req.id })).phase == "timed_out"
  end)
  MiniTest.expect.equality(children[1].disposed, true)
  MiniTest.expect.equality(next(children[1].listeners), nil)
  children[1].ready(children[1])
  nvim.wait(20, function()
    return false
  end)
  MiniTest.expect.equality(#children[1].prompts, 0)
  MiniTest.expect.equality(assert(control:request(req)).phase, "timed_out")
  MiniTest.expect.equality(#children, 1)
end

T["unlimited operations allocate no deadline and still release their observers"] = function()
  for _, ending in ipairs({ "complete", "cancel", "dispose" }) do
    local control, children, _, ready = fixture()
    local req = request("unlimited-" .. ending)
    req.timeout_ms = false
    assert(control:request(req))
    MiniTest.expect.equality(control.operations[req.id].timer, nil)
    wait_for(function()
      return #children == 1
    end)
    ready(1)
    wait_for(function()
      return #children[1].prompts == 1
    end)
    if ending == "complete" then
      children[1].state.status = "ready"
      children[1].complete({ stopReason = "end_turn" })
    elseif ending == "cancel" then
      assert(control:request({ action = "cancel", id = req.id }))
    else
      control:dispose()
    end
    wait_for(function()
      return control.operations[req.id].terminal
    end)
    MiniTest.expect.equality(next(children[1].listeners), nil)
    MiniTest.expect.equality(control.operations[req.id].timer, nil)
    MiniTest.expect.equality(
      control.operations[req.id].snapshot.phase,
      ending == "complete" and "completed" or "cancelled"
    )
  end
end

T["omitted deadlines remain finite and malformed deadlines create nothing"] = function()
  local control, children = fixture()
  for _, timeout in ipairs({ true, 0, -1, 1.5, 3600001, "unlimited" }) do
    local req = request("invalid-deadline")
    req.timeout_ms = timeout
    local state, err = control:request(req)
    MiniTest.expect.equality(state, nil)
    assert(assert(err):find("timeout_ms", 1, true))
  end
  MiniTest.expect.equality(control.count, 0)
  MiniTest.expect.equality(#children, 0)
  assert(control:request(request("default-deadline")))
  assert(control.operations["default-deadline"].timer ~= nil)
end

T["running deadline retains the reason and final child status without replay"] = function()
  -- Observed: codex/01a13881-37a8-7c93-a011-777c7ab3edc4, operation
  -- 01a1387a-b795-7113-b3a2-c6f6b5a54465:next-1, timeout_ms=3600000.
  -- Proxy sessions/<ACP id>/log.jsonl:2565 exits at 04:42:34.210Z.
  -- The controller said timed_out/status=prompting; its chat showed Unavailable.
  local control, children, _, ready = fixture()
  local req = request("running-deadline")
  req.timeout_ms = 100
  assert(control:request(req))
  wait_for(function()
    return #children == 1
  end)
  ready(1)
  wait_for(function()
    return #children[1].prompts == 1
  end)
  local child = children[1]
  child.state.status = "prompting"
  child:emit("admission_settled", { result = "dispatched" })
  wait_for(function()
    return assert(control:request({ action = "inspect", id = req.id })).phase == "timed_out"
  end)
  local snapshot = assert(control:request({ action = "inspect", id = req.id }))
  MiniTest.expect.equality(snapshot.status, "disposed")
  MiniTest.expect.equality(child:inspect().disposal_reason, "Timed out after 100 ms")
  MiniTest.expect.equality(snapshot.error, "Timed out after 100 ms")
  child.complete({ stopReason = "end_turn" })
  assert(control:request(req))
  MiniTest.expect.equality(#children, 1)
  MiniTest.expect.equality(#child.prompts, 1)
  MiniTest.expect.equality(assert(control:request({ action = "inspect", id = req.id })).phase, "timed_out")
end

T["controller disposal prevents an already queued option acknowledgment from sending"] = function()
  local control, children, _, ready = fixture()
  local req = request("dispose-queued")
  req.options = { { id = "model", value = "b" } }
  assert(control:request(req))
  wait_for(function()
    return #children == 1
  end)
  ready(1, options("a"))
  wait_for(function()
    return #children[1].changes == 1
  end)
  acknowledge(children[1], 1, options("b"))
  control:dispose()
  nvim.wait(20, function()
    return false
  end)
  MiniTest.expect.equality(children[1].disposed, true)
  MiniTest.expect.equality(next(children[1].listeners), nil)
  MiniTest.expect.equality(#children[1].prompts, 0)
end

T["configured entrypoint runs two real mock children without changing the current draft"] = function()
  local plugin = require("louiselm")
  local Command = require("louiselm.ui.chat.command")
  local root = nvim.fn.getcwd()
  MiniTest.finally(function()
    Command.register()
    Command.configure(nil)
  end)
  assert(plugin.setup({
    agents = {
      mock = {
        provider = "test-service",
        command = nvim.v.progpath,
        args = {
          "--headless",
          "--noplugin",
          "-i",
          "NONE",
          "--cmd",
          "set runtimepath^=" .. nvim.fn.fnameescape(root),
          "-u",
          root .. "/tests/mock/init.lua",
          "-c",
          "lua require('louiselm.dev.mock_agent').run()",
        },
      },
    },
  }))
  local parent = nvim.api.nvim_create_buf(true, false)
  MiniTest.finally(function()
    if nvim.api.nvim_buf_is_valid(parent) then
      nvim.api.nvim_buf_delete(parent, { force = true })
    end
  end)
  nvim.api.nvim_set_current_buf(parent)
  nvim.api.nvim_buf_set_lines(parent, 0, -1, false, { "human draft stays here" })
  local first, second = request("real-first"), request("real-second")
  first.timeout_ms = false
  first.cwd, second.cwd = root, root .. "/tests"
  assert(plugin.session_control(first))
  assert(plugin.session_control(second))
  local completed = nvim.wait(10000, function()
    local one = assert(plugin.session_control({ action = "inspect", id = first.id }))
    local two = assert(plugin.session_control({ action = "inspect", id = second.id }))
    return one.phase == "completed" and two.phase == "completed"
  end, 10)
  assert(completed, "real child operations did not complete: " .. nvim.inspect({
    first = plugin.session_control({ action = "inspect", id = first.id }),
    second = plugin.session_control({ action = "inspect", id = second.id }),
  }))
  for _, req in ipairs({ first, second }) do
    local state = assert(plugin.session_control({ action = "inspect", id = req.id, result = true }))
    MiniTest.expect.equality(state.admission, "dispatched")
    MiniTest.expect.equality(state.result.text, req.prompt)
    local lines = nvim.api.nvim_buf_get_lines(state.buffer, 0, -1, false)
    local count = 0
    for _, line in ipairs(lines) do
      if line == "> " .. req.prompt then
        count = count + 1
      end
    end
    MiniTest.expect.equality(count, 1)
  end
  MiniTest.expect.equality(nvim.api.nvim_get_current_buf(), parent)
  MiniTest.expect.equality(nvim.api.nvim_buf_get_lines(parent, 0, -1, false), { "human draft stays here" })
  Command.register()
  local missing, err = plugin.session_control({ action = "inspect", id = first.id })
  MiniTest.expect.equality(missing, nil)
  assert(assert(err):find("unknown", 1, true))
end

return T
