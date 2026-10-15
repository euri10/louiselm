local Draft = require("louiselm.ui.chat.draft")
---@diagnostic disable-next-line: undefined-global -- Neovim owns this controller.
local nvim = vim

---@class louiselm.ui.SessionControlOperation
---@field request louiselm.SessionControlRequest Owned exact retry input.
---@field snapshot louiselm.SessionControlSnapshot
---@field session? louiselm.session.Session
---@field timer? {stop: fun(self: table), close: fun(self: table), is_closing: fun(self: table): boolean}
---@field unsubscribe? fun()
---@field terminal boolean
---@field output string Bounded assistant text, never included by default.
---@field truncated boolean
---@field cancel_requested? boolean Cancellation was requested by the operation owner.
---@field completion_received? boolean Peer completion arrived; scheduled settlement must win a later cancel.
---@field completion_error? string Completion error waiting for cancellation admission settlement.

---@class louiselm.ui.SessionControl
---@field api louiselm.session.Api
---@field attach fun(session: louiselm.session.Session): integer?, string?
---@field operations table<string, louiselm.ui.SessionControlOperation>
---@field count integer
---@field disposed boolean
---@field skill_catalog? string Configured injected catalog, used only by inject-policy children.
---@field request fun(self: louiselm.ui.SessionControl, request: unknown): louiselm.SessionControlSnapshot?, string?
---@field dispose fun(self: louiselm.ui.SessionControl)

local M = {}
local Control = {}
Control.__index = Control
local START_FIELDS = {
  action = true,
  id = true,
  agent = true,
  cwd = true,
  name = true,
  prompt = true,
  context = true,
  options = true,
  parent_turn_id = true,
  timeout_ms = true,
}
local ACTIVE = {
  admitting = true,
  preparing = true,
  prompting = true,
  running = true,
  waiting_permission = true,
  cancelling = true,
}

local function fields(value, allowed, label)
  if type(value) ~= "table" then
    return label .. " must be an object"
  end
  for key in pairs(value) do
    if not allowed[key] then
      return label .. " has unknown field " .. tostring(key)
    end
  end
end

local function text(value, maximum)
  return type(value) == "string" and #value > 0 and #value <= maximum and not value:find("%z")
end

local function array(value, limit, label)
  if type(value) ~= "table" then
    return label .. " must be an array"
  end
  local count = 0
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
      return label .. " must be a dense array"
    end
    count = count + 1
  end
  if count > limit then
    return label .. " must be dense with at most " .. limit .. " entries"
  end
  for index = 1, count do
    if value[index] == nil then
      return label .. " must be a dense array"
    end
  end
end

---Validate the untouched request before constructing owners or reserving keys.
---@param value unknown Decoded external request.
---@return string? error_message
function M.validate(value)
  if type(value) ~= "table" then
    return "request must be an object"
  end
  if value.action == "discover" then
    local err = fields(value, { action = true, agent = true, session_id = true, option_id = true }, "request")
    if err then
      return err
    end
    for _, key in ipairs({ "agent", "session_id", "option_id" }) do
      if value[key] ~= nil and not text(value[key], 128) then
        return key .. " must be a non-empty string of at most 128 bytes"
      end
    end
    if value.session_id and not value.agent then
      return "session_id requires agent"
    end
    if value.option_id and not value.session_id then
      return "option_id requires session_id"
    end
    return
  end
  if value.action ~= "start" and value.action ~= "inspect" and value.action ~= "cancel" then
    return "action must be start, inspect, cancel or discover"
  end
  local allowed = value.action == "start" and START_FIELDS
    or { action = true, id = true, result = value.action == "inspect" }
  local err = fields(value, allowed, "request")
  if err then
    return err
  end
  if not text(value.id, 128) then
    return "id must be a non-empty string of at most 128 bytes"
  end
  if value.result ~= nil and type(value.result) ~= "boolean" then
    return "result must be boolean"
  end
  if value.action ~= "start" then
    return
  end
  if not text(value.agent, 128) then
    return "agent must be a configured Agent name"
  end
  if not text(value.cwd, 4096) or value.cwd:sub(1, 1) ~= "/" then
    return "cwd must be an absolute directory"
  end
  if not text(value.prompt, 65536) or not value.prompt:find("%S") then
    return "prompt must contain text, at most 65536 bytes"
  end
  if value.name ~= nil and (not text(value.name, 256) or value.name:find("[\r\n]")) then
    return "name must be a non-empty single line, at most 256 bytes"
  end
  if value.parent_turn_id ~= nil and not text(value.parent_turn_id, 128) then
    return "parent_turn_id must be a non-empty turn identity"
  end
  if
    value.timeout_ms ~= nil
    and value.timeout_ms ~= false
    and (
      type(value.timeout_ms) ~= "number"
      or value.timeout_ms % 1 ~= 0
      or value.timeout_ms < 1
      or value.timeout_ms > 3600000
    )
  then
    return "timeout_ms must be false (no deadline) or an integer from 1 to 3600000"
  end
  local seen = {}
  if value.options ~= nil then
    err = array(value.options, 32, "options")
    if err then
      return err
    end
    for _, option in ipairs(value.options) do
      err = fields(option, { id = true, value = true }, "option")
      if err then
        return err
      end
      if not text(option.id, 128) or not (type(option.value) == "boolean" or text(option.value, 256)) then
        return "option requires id and a string or boolean value"
      end
      if seen[option.id] then
        return "duplicate option id " .. option.id
      end
      seen[option.id] = true
    end
  end
  if value.context ~= nil then
    err = array(value.context, 16, "context")
    if err then
      return err
    end
    local bytes = #value.prompt
    for _, item in ipairs(value.context) do
      err = fields(item, { name = true, text = true }, "context item")
      if err then
        return err
      end
      if not text(item.name, 256) or item.name:find("[\r\n]") or not text(item.text, 262144) then
        return "context requires a single-line name and non-empty text"
      end
      bytes = bytes + #item.text + #item.name
    end
    if bytes > 262144 then
      return "prompt and context exceed 262144 bytes"
    end
  end
end

local function values(options)
  local result = {}
  for _, option in ipairs(options) do
    result[option.id] = option.current_value
  end
  return result
end

---Read existing advertisements only; never construct a Session, change options or open UI.
---@param api? louiselm.session.Api Existing configured owner, if already initialized.
---@param definitions table Configured Agent definitions; only their names are returned.
---@param request louiselm.SessionControlDiscoveryRequest Validated selectors.
---@return louiselm.SessionControlDiscovery? discovery Bounded metadata, not a guarantee of future choices.
---@return string? error_message Unknown selector or oversized advertisement.
function M.discover(api, definitions, request)
  if request.agent and not definitions[request.agent] then
    return nil, "unknown configured Agent"
  end
  local result = { agents = {}, sessions = {}, availability = "unavailable", truncated = false }
  for name in pairs(definitions) do
    if not request.agent or name == request.agent then
      result.agents[#result.agents + 1] = name
    end
  end
  table.sort(result.agents)
  if #result.agents > 128 then
    return nil, "too many configured Agents; select agent"
  end
  if request.agent and api then
    for _, id in ipairs(request.session_id and { request.session_id } or api:list_sessions()) do
      local session = api:get_session(id)
      local state = session and session:inspect()
      if request.session_id and (not state or state.agent ~= request.agent) then
        return nil, "Session is unavailable or belongs to a different Agent"
      end
      if state and state.agent == request.agent then
        if #result.sessions == 32 then
          result.truncated = true
          break
        end
        result.sessions[#result.sessions + 1] = {
          session_id = state.id,
          acp_session_id = state.acp_session_id,
          status = state.status,
          options = values(state.config_options),
        }
        result.availability = "select_session"
        if request.session_id then
          result.availability = "unavailable"
          if state.status ~= "disposed" and state.status ~= "starting" and #state.config_options > 0 then
            result.options = {}
            for _, option in ipairs(state.config_options) do
              if not request.option_id or option.id == request.option_id then
                local selected = {
                  id = option.id,
                  name = option.name,
                  category = option.category,
                  type = option.type,
                  current_value = option.current_value,
                }
                if option.options then
                  selected.options = {}
                  for _, choice in ipairs(option.options) do
                    selected.options[#selected.options + 1] = { name = choice.name, value = choice.value }
                  end
                end
                result.options[#result.options + 1] = selected
              end
            end
            if request.option_id and #result.options == 0 then
              return nil, "option is not advertised by this Session"
            end
            result.availability = "available"
          end
        end
      end
    end
  elseif request.session_id then
    return nil, "Session advertisements are unavailable; no configured owner is initialized"
  end
  if #nvim.json.encode(result) > 65536 then
    return nil, "discovery exceeds 64 KiB; select agent, session_id and option_id"
  end
  return result
end

---@param timeout_ms integer
---@return string reason
local function deadline_reason(timeout_ms)
  for _, unit in ipairs({ { 3600000, "hour" }, { 60000, "minute" }, { 1000, "second" }, { 1, "ms" } }) do
    if timeout_ms % unit[1] == 0 then
      local count = timeout_ms / unit[1]
      return string.format("Timed out after %d %s%s", count, unit[2], count ~= 1 and unit[2] ~= "ms" and "s" or "")
    end
  end
  error("validated deadline has no unit")
end

---@param op louiselm.ui.SessionControlOperation
---@param changes? table
local function update(op, changes)
  local state = nvim.deepcopy(op.snapshot)
  if not op.terminal and op.session then
    local child = op.session:inspect()
    state.session_id, state.acp_session_id = child.id, child.acp_session_id
    state.status, state.current_turn = child.status, child.current_turn
    state.options = values(child.config_options)
  end
  for key, value in pairs(changes or {}) do
    state[key] = value
  end
  if not nvim.deep_equal(state, op.snapshot) then
    state.revision = state.revision + 1
    op.snapshot = state
  end
end

---@param op louiselm.ui.SessionControlOperation
local function release(op)
  if op.unsubscribe then
    op.unsubscribe()
    op.unsubscribe = nil
  end
  if op.timer then
    if not op.timer:is_closing() then
      op.timer:stop()
      op.timer:close()
    end
    op.timer = nil
  end
end

---@param op louiselm.ui.SessionControlOperation
---@param phase string
---@param err? string
local function finish(op, phase, err)
  if op.terminal then
    return
  end
  if err and #err > 1024 then
    local last = 1024
    while last > 0 and err:byte(last + 1) >= 128 and err:byte(last + 1) < 192 do
      last = last - 1
    end
    err = err:sub(1, last)
  end
  update(op, { phase = phase, error = err })
  op.terminal = true
  release(op)
end

---@param self louiselm.ui.SessionControl
---@param op louiselm.ui.SessionControlOperation
---@param callback fun()
local function scheduled(self, op, callback)
  nvim.schedule(function()
    if not self.disposed and not op.terminal then
      callback()
    end
  end)
end

---@param self louiselm.ui.SessionControl
---@param op louiselm.ui.SessionControlOperation
---@param event louiselm.session.Event
local function observe(self, op, event)
  scheduled(self, op, function()
    if event.type == "chunk" then
      local content = event.data.content
      if type(content) == "table" and content.type == "text" and type(content.text) == "string" then
        local combined = op.output .. content.text
        op.truncated = op.truncated or #combined > 8192
        local first = math.max(1, #combined - 8191)
        -- A bounded tail must still be valid UTF-8 for JSON and the next reader.
        while first <= #combined and combined:byte(first) >= 128 and combined:byte(first) < 192 do
          first = first + 1
        end
        op.output = combined:sub(first)
      end
    elseif event.type == "admission_settled" then
      update(op, { admission = event.data.result })
      if event.data.result == "dispatched" then
        update(op, { phase = "running" })
      elseif event.data.result == "cancelled" then
        finish(op, "cancelled")
      elseif op.completion_error then
        finish(op, "failed", op.completion_error)
      end
    elseif event.type == "error" then
      finish(op, "failed", event.data.message)
    else
      update(op)
      if op.snapshot.status == "disposed" then
        finish(op, "cancelled")
      end
    end
  end)
end

---@param self louiselm.ui.SessionControl
---@param op louiselm.ui.SessionControlOperation
local function submit(self, op)
  local session = op.session
  if not session then
    return
  end
  local state = session:inspect()
  if state.status ~= "ready" or state.current_turn ~= 0 then
    finish(op, "failed", "child is no longer idle and empty")
    return
  end
  local current = values(state.config_options)
  for _, option in ipairs(op.request.options or {}) do
    if current[option.id] ~= option.value then
      finish(op, "failed", "child did not confirm requested option " .. option.id)
      return
    end
  end
  local draft = Draft.new()
  if state.skills_policy == "inject" and self.skill_catalog ~= nil then
    draft:set_catalog(self.skill_catalog)
  end
  for _, item in ipairs(op.request.context or {}) do
    draft:add_context({ label = item.name, text = item.text })
  end
  local content = draft:with_context(op.request.prompt, state.embedded_context)
  update(op, { phase = "admitting" })
  local turn, err = session:prompt(content, function(result, failure)
    -- Record arrival before scheduling: the Session may already be idle while
    -- the UI callback is queued. A later cancel must not dispose that result.
    op.completion_received = true
    scheduled(self, op, function()
      if failure and op.cancel_requested and op.snapshot.admission == nil then
        op.completion_error = failure
        return
      end
      if failure then
        finish(op, "failed", failure)
        return
      end
      local reason = type(result) == "table" and result.stopReason or nil
      if type(reason) == "string" then
        update(op, { stop_reason = reason })
      end
      finish(op, reason == "cancelled" and "cancelled" or "completed")
    end)
  end, { parent_turn_id = op.request.parent_turn_id })
  if not turn then
    finish(op, "failed", err or "prompt admission failed")
    return
  end
  update(op, { turn_id = turn })
end

---@param self louiselm.ui.SessionControl
---@param op louiselm.ui.SessionControlOperation
---@param index integer
local function configure(self, op, index)
  local option = (op.request.options or {})[index]
  if not option then
    submit(self, op)
    return
  end
  local session = op.session
  if not session then
    return
  end
  update(op, { phase = "configuring" })
  local request_id, err = session:set_config_option(option.id, option.value, function(replacement, failure)
    scheduled(self, op, function()
      if not replacement then
        finish(op, "failed", failure or "option change failed")
        return
      end
      if values(replacement)[option.id] ~= option.value then
        finish(op, "failed", "Agent did not confirm requested option " .. option.id)
        return
      end
      configure(self, op, index + 1)
    end)
  end)
  if not request_id then
    finish(op, "failed", err or "option change refused")
  end
end

---@param self louiselm.ui.SessionControl
---@param op louiselm.ui.SessionControlOperation
local function start(self, op)
  local session, err = self.api:create_session(op.request.agent, {
    cwd = op.request.cwd,
    name = op.request.name,
  }, function(ready, failure)
    scheduled(self, op, function()
      if not ready then
        finish(op, "failed", failure or "child initialization failed")
        return
      end
      configure(self, op, 1)
    end)
  end)
  if not session then
    finish(op, "failed", err or "child creation failed")
    return
  end
  op.session = session
  op.unsubscribe = session:on(function(event)
    observe(self, op, event)
  end)
  local buffer, attach_error = self.attach(session)
  if not buffer then
    finish(op, "failed", attach_error or "child attachment failed")
    local disposed, dispose_error = session:dispose()
    if not disposed then
      update(op, { error = dispose_error or "child disposal failed" })
    end
    return
  end
  update(op, { buffer = buffer })
end

---Own bounded delegation records over an existing configured Session API.
---The caller owns the API and supplies background Chat attachment on the main loop.
---@param api louiselm.session.Api Existing configured owner; never reconstructed from process state.
---@param attach fun(session: louiselm.session.Session): integer?, string? Attach without changing focus.
---@param skill_catalog? string Existing configured injection catalog; never applied to native-policy children.
---@return louiselm.ui.SessionControl controller Explicitly disposed by the command registration.
function M.new(api, attach, skill_catalog)
  return setmetatable(
    { api = api, attach = attach, skill_catalog = skill_catalog, operations = {}, count = 0, disposed = false },
    Control
  )
end

---Apply a validated data request. Exact start retries return the original operation.
---Keys and bounded result tails live only for this controller's lifetime; no disk logs.
---@param request unknown Untrusted decoded request; validated before side effects.
---@return louiselm.SessionControlSnapshot? snapshot Owned compact snapshot.
---@return string? error_message Specific validation, unknown-key or retry-conflict error.
function Control:request(request)
  if self.disposed then
    return nil, "Session controls are disposed"
  end
  local err = M.validate(request)
  if err then
    return nil, err
  end
  ---@cast request louiselm.SessionControlRequest
  local op = self.operations[request.id]
  if request.action == "start" then
    if op then
      if not nvim.deep_equal(op.request, request) then
        return nil, "operation id already has a different request"
      end
    else
      if self.count >= 128 then
        return nil, "Session control operation limit (128) reached for this editor lifetime"
      end
      op = {
        request = nvim.deepcopy(request),
        snapshot = { id = request.id, revision = 1, phase = "starting", options = {} },
        terminal = false,
        output = "",
        truncated = false,
      }
      self.operations[request.id] = op
      self.count = self.count + 1
      local created = op
      if request.timeout_ms ~= false then
        local timeout_ms = request.timeout_ms or 300000
        created.timer = nvim.defer_fn(function()
          if self.disposed or created.terminal or created.completion_received then
            return
          end
          local reason = deadline_reason(timeout_ms)
          finish(created, "timed_out", reason)
          if created.session then
            local disposed, dispose_error = created.session:dispose(reason)
            update(created, { status = created.session:inspect().status })
            if not disposed then
              update(created, { error = dispose_error or "deadline disposal failed" })
            end
          end
        end, timeout_ms)
      end
      scheduled(self, created, function()
        start(self, created)
      end)
    end
  elseif not op then
    return nil, "unknown operation id in this editor registration; do not automatically replay after restart"
  elseif request.action == "cancel" and not op.terminal and not op.completion_received then
    op.cancel_requested = true
    local session = op.session
    if session and ACTIVE[session:inspect().status] then
      local cancelled, cancel_error = session:cancel()
      if not cancelled then
        finish(op, "failed", cancel_error or "cancellation failed")
      else
        update(op, { phase = "cancelling" })
      end
    else
      finish(op, "cancelled")
      if session then
        local disposed, dispose_error = session:dispose()
        if not disposed then
          update(op, { error = dispose_error or "child disposal failed" })
        end
      end
    end
  end
  local snapshot = nvim.deepcopy(op.snapshot)
  if request.result then
    snapshot.result = { text = op.output, truncated = op.truncated }
  end
  return snapshot
end

---Release observers/timers and stop this controller's still-active children.
---Completed children remain owned by the Chat/API for human follow-up.
function Control:dispose()
  if self.disposed then
    return
  end
  self.disposed = true
  for _, op in pairs(self.operations) do
    if not op.terminal then
      finish(op, "cancelled")
      if op.session then
        local disposed, err = op.session:dispose()
        if not disposed then
          update(op, { error = err or "child disposal failed" })
        end
      end
    end
  end
end

return M
