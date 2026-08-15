local Acp = require("louiselm.acp")
local Permission = require("louiselm.permission")
local Events = require("louiselm.session.events")
local Validation = require("louiselm.session.validation")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@alias louiselm.session.Status "starting"|"ready"|"configuring"|"prompting"|"waiting_permission"|"cancelling"|"error"|"disposed"
---@alias louiselm.session.Prompt string|table

---@class louiselm.session.State
---@field id string Local session identifier.
---@field name string User-facing session name.
---@field source "new"|"loaded" Whether the session was created or restored.
---@field agent string Named agent definition.
---@field acp_session_id? string Agent-side persistent conversation identifier, once available.
---@field status louiselm.session.Status Lifecycle state.
---@field working_dir string ACP working directory.
---@field current_turn integer Number of the current or most recently completed turn.
---@field config_options louiselm.session.ConfigOption[] Supported agent-advertised options in priority order.
---@field context? louiselm.session.ContextUsage Latest agent-reported context state.
---@field cost? louiselm.session.Cost Latest agent-reported cumulative cost.
---@field usage? louiselm.session.TurnUsage Latest agent-reported completed-turn usage.
---@field activity? string Current generic tool activity.
---@field skills_policy louiselm.skills.Policy Effective session-static Agent Skills policy.

---@class louiselm.session.Options
---@field cwd? string Working directory for the ACP session.
---@field name? string User-facing session name.
---@field on_event? louiselm.session.EventCallback Initial event listener.
---@field permission_policy? louiselm.permission.Policy Policy for agent-requested operations.
---@field permission_store? louiselm.permission.Store Remembered-permission owner.

---@class louiselm.session.Session
---@field state louiselm.session.State Internal mutable state.
---@field emitter louiselm.session.EventEmitter Event subscribers.
---@field client louiselm.acp.Client? ACP client.
---@field acp_session_id string? Agent-side session identifier.
---@field load_session_id string? Agent-side session identifier to load.
---@field prompt_callback? fun(result: unknown, error?: string) Current prompt completion callback.
---@field ready_callback? fun(session: louiselm.session.Session?, error?: string) Session startup callback.
---@field ready_callback_called boolean Whether startup callback ran.
---@field turn_done_turn integer? Turn for which the completion event was emitted.
---@field owner louiselm.session.Registry Registry that owns this session.
---@field definition louiselm.agent.Definition Agent process definition.
---@field options louiselm.session.Options Session options.
---@field permission_policy louiselm.permission.Policy Policy for agent-requested operations.
---@field permission_store louiselm.permission.Store Remembered-permission owner.
---@field start fun(self: louiselm.session.Session): boolean, string?
---@field on fun(self: louiselm.session.Session, callback: louiselm.session.EventCallback): fun()
---@field inspect fun(self: louiselm.session.Session): louiselm.session.State
---@field set_name fun(self: louiselm.session.Session, name: string): boolean, string? Rename the session.
---@field prompt fun(self: louiselm.session.Session, prompt: louiselm.session.Prompt, callback?: fun(result: unknown, error?: string)): string|number?, string?
---@field cancel fun(self: louiselm.session.Session): boolean, string?
---@field set_config_option fun(self: louiselm.session.Session, id: string, value: string|boolean, callback?: fun(options: louiselm.session.ConfigOption[]?, error?: string)): string|number?, string?
---@field dispose fun(self: louiselm.session.Session): boolean, string?

local M = {}
local Session = {}
Session.__index = Session

---@param value unknown
---@return unknown copy
local function copy(value)
  if type(value) ~= "table" then
    return value
  end
  local result = {}
  for key, item in pairs(value) do
    result[key] = copy(item)
  end
  return result
end

---@param error_value louiselm.acp.JsonRpcError|string|nil
---@return string
local function error_message(error_value)
  if type(error_value) == "table" then
    return error_value.message
  end
  return tostring(error_value or "ACP request failed")
end

---@param self louiselm.session.Session
---@param event_type louiselm.session.EventType
---@param data unknown
---@param respond? fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
local function emit(self, event_type, data, respond)
  if self.state.status == "disposed" then
    return
  end
  if respond ~= nil then
    self.emitter:emit({ type = event_type, session_id = self.state.id, data = data, respond = respond })
  else
    self.emitter:emit({ type = event_type, session_id = self.state.id, data = data })
  end
end

---@param self louiselm.session.Session
---@param status louiselm.session.Status
local function set_status(self, status)
  if self.state.status == status then
    return
  end
  self.state.status = status
  emit(self, "state_changed", { status = status, activity = self.state.activity })
end

---@param self louiselm.session.Session
---@param message string
local function fail(self, message)
  if self.state.status == "disposed" or self.state.status == "error" then
    return
  end
  set_status(self, "error")
  local client = self.client
  if client ~= nil then
    local closed, close_error = client:close()
    if not closed then
      message = message .. "; ACP cleanup failed: " .. (close_error or "unknown error")
    end
  end
  local prompt_callback = self.prompt_callback
  self.prompt_callback = nil
  emit(self, "error", { message = message })
  if prompt_callback ~= nil then
    prompt_callback(nil, message)
  end
  if not self.ready_callback_called then
    self.ready_callback_called = true
    local callback = self.ready_callback
    if callback ~= nil then
      callback(nil, message)
    end
  end
end

---@param self louiselm.session.Session
---@param result unknown
local function complete_turn(self, result)
  if self.state.status == "disposed" or self.state.status == "error" then
    return
  end
  if self.turn_done_turn ~= self.state.current_turn then
    self.turn_done_turn = self.state.current_turn
    set_status(self, "ready")
    emit(self, "turn_done", result)
  end
  local callback = self.prompt_callback
  self.prompt_callback = nil
  if callback ~= nil then
    callback(result)
  end
end

---@param self louiselm.session.Session
---@param message louiselm.acp.JsonRpcNotification
local function handle_notification(self, message)
  if message.method ~= "session/update" or type(message.params) ~= "table" then
    return
  end
  local params = message.params
  ---@cast params table
  if params.sessionId ~= self.acp_session_id then
    return
  end
  local update = params.update
  if type(update) ~= "table" or type(update.sessionUpdate) ~= "string" then
    fail(self, "malformed ACP session/update notification")
    return
  end

  local update_type = update.sessionUpdate
  if update_type == "agent_message_chunk" then
    emit(self, "chunk", update)
  elseif update_type == "tool_call" or update_type == "tool_call_update" then
    local status = update.status
    if status == "completed" or status == "failed" or status == "cancelled" then
      self.state.activity = nil
      emit(self, "tool_call_finished", update)
    else
      self.state.activity = type(update.title) == "string" and update.title or "tool"
      emit(self, "tool_call_started", update)
    end
    emit(self, "state_changed", { status = self.state.status, activity = self.state.activity })
  elseif update_type == "config_option_update" then
    local options = Validation.config_options(update.configOptions)
    if options == nil then
      fail(self, "malformed ACP config_option_update notification")
      return
    end
    local previous_model = Validation.model_value(self.state.config_options)
    self.state.config_options = options
    local current_model = Validation.model_value(options)
    if self.state.context ~= nil and previous_model ~= nil and previous_model ~= current_model then
      self.state.context.stale = true
    end
    emit(self, "config_options_changed", copy(options))
  elseif update_type == "usage_update" then
    local context, cost, cost_present = Validation.usage_update(update)
    if context == nil then
      fail(self, "malformed ACP usage_update notification")
      return
    end
    self.state.context = context
    if cost_present then
      self.state.cost = cost
    end
    emit(self, "usage_updated", copy({ context = context, cost = self.state.cost }))
  end
end

---@param self louiselm.session.Session
---@param request louiselm.acp.JsonRpcRequest
---@param respond fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?
local function handle_request(self, request, respond)
  if request.method ~= "session/request_permission" then
    return
  end
  if type(request.params) ~= "table" or request.params.sessionId ~= self.acp_session_id then
    respond(nil, { code = -32602, message = "malformed ACP permission request" })
    return
  end
  local data = {}
  for key, value in pairs(request.params) do
    data[key] = value
  end
  data.request_id = request.id
  data.operation = Permission.gates.from_acp(data)
  local decision, policy_error = Permission.gates.check(self.permission_policy, data.operation)
  if decision == nil then
    respond(nil, {
      code = -32602,
      message = "invalid ACP permission request: " .. (policy_error or "permission policy failed"),
    })
    return
  end
  data.policy_decision = decision
  if decision == "ask" then
    local remembered, remembered_error = self.permission_store:evaluate({
      session_id = self.state.id,
      agent = self.state.agent,
      adapter = { command = self.definition.command, args = self.definition.args },
      workspace = self.state.working_dir,
    }, data.operation)
    if remembered == nil then
      data.permission_error = remembered_error or "remembered permission lookup failed"
    elseif remembered ~= "ask" then
      local remembered_response = Permission.gates.remembered_response(data, remembered)
      if remembered_response ~= nil then
        respond(remembered_response)
        return
      end
      data.remembered_decision = remembered
    end
  end
  if decision ~= "ask" then
    local automatic_response = Permission.gates.response(data, decision)
    if automatic_response ~= nil then
      respond(automatic_response)
      return
    end
  end
  set_status(self, "waiting_permission")
  local function permission_respond(result, rpc_error)
    local selected_decision, lifetime = Permission.gates.remembered_choice(data, result)
    if selected_decision ~= nil and lifetime ~= nil then
      local _, remember_error = self.permission_store:remember({
        session_id = self.state.id,
        agent = self.state.agent,
        adapter = { command = self.definition.command, args = self.definition.args },
        workspace = self.state.working_dir,
      }, data.operation, selected_decision, lifetime)
      if remember_error ~= nil then
        local cancelled, cancel_error = respond({ outcome = { outcome = "cancelled" } })
        if cancelled and self.state.status == "waiting_permission" then
          set_status(self, "prompting")
        end
        local message = "permission choice was cancelled because it could not be remembered: " .. remember_error
        if not cancelled then
          message = message .. "; cancellation failed: " .. (cancel_error or "unknown error")
        end
        return false, message
      end
    end
    local sent, send_error = respond(result, rpc_error)
    if sent and self.state.status == "waiting_permission" then
      set_status(self, "prompting")
    end
    return sent, send_error
  end
  emit(self, "permission_requested", data, permission_respond)
end

---@param self louiselm.session.Session
---@param result louiselm.agent.ProcessResult
local function handle_exit(self, result)
  if self.state.status == "disposed" then
    return
  end
  if result.signal ~= nil and result.signal ~= 0 then
    fail(self, "agent process exited with signal " .. result.signal)
  else
    fail(self, "agent process exited with code " .. tostring(result.code))
  end
end

---@param self louiselm.session.Session
---@param result unknown
---@param rpc_error? louiselm.acp.JsonRpcError
local function handle_initialized(self, result, rpc_error)
  if self.state.status == "disposed" then
    return
  end
  if rpc_error ~= nil then
    fail(self, "ACP initialize failed: " .. error_message(rpc_error))
    return
  end
  if type(result) ~= "table" then
    fail(self, "ACP initialize returned a malformed result")
    return
  end
  local client = self.client
  if client == nil then
    fail(self, "ACP client disappeared during initialization")
    return
  end
  local method = self.load_session_id == nil and "new" or "load"
  local request_id, request_error
  local on_session_ready = function(session_result, session_error)
    if self.state.status == "disposed" then
      return
    end
    if session_error ~= nil then
      fail(self, "ACP session/" .. method .. " failed: " .. error_message(session_error))
      return
    end
    if self.load_session_id == nil then
      if
        type(session_result) ~= "table"
        or type(session_result.sessionId) ~= "string"
        or session_result.sessionId == ""
      then
        fail(self, "ACP session/new returned a malformed result")
        return
      end
      self.acp_session_id = session_result.sessionId
    elseif type(session_result) ~= "table" or session_result == nvim.NIL then
      fail(self, "ACP session/load returned a malformed result")
      return
    end
    local options, options_error
    if self.load_session_id ~= nil and session_result.configOptions == nil then
      options = copy(self.state.config_options)
    else
      local config_options
      if type(session_result) == "table" and session_result ~= nvim.NIL then
        config_options = session_result.configOptions
      end
      options, options_error = Validation.config_options(config_options)
    end
    if options == nil then
      fail(
        self,
        "ACP session/" .. method .. " returned malformed configOptions: " .. (options_error or "invalid options")
      )
      return
    end
    self.state.config_options = options
    set_status(self, "ready")
    if not self.ready_callback_called then
      self.ready_callback_called = true
      local callback = self.ready_callback
      if callback ~= nil then
        callback(self)
      end
    end
  end
  if self.load_session_id == nil then
    request_id, request_error = client:new_session({ cwd = self.state.working_dir, mcpServers = {} }, on_session_ready)
  else
    self.acp_session_id = self.load_session_id
    request_id, request_error = client:load_session(
      { sessionId = self.load_session_id, cwd = self.state.working_dir, mcpServers = {} },
      on_session_ready
    )
  end
  if request_id == nil then
    fail(self, "ACP session/" .. method .. " failed: " .. (request_error or "request could not be sent"))
  end
end

---@param self louiselm.session.Session
---@param result unknown
---@param rpc_error? louiselm.acp.JsonRpcError
local function handle_prompt_result(self, result, rpc_error)
  if self.state.status == "disposed" then
    return
  end
  if rpc_error ~= nil then
    local message = "ACP session/prompt failed: " .. error_message(rpc_error)
    fail(self, message)
    return
  end
  if type(result) ~= "table" then
    fail(self, "ACP session/prompt returned a malformed result")
    return
  end
  local usage, usage_valid = Validation.turn_usage(result.usage)
  if not usage_valid then
    fail(self, "ACP session/prompt returned malformed usage")
    return
  end
  self.state.usage = usage
  complete_turn(self, result)
end

---@param owner louiselm.session.Registry
---@param id string
---@param agent_name string
---@param definition louiselm.agent.Definition
---@param options louiselm.session.Options
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string)
---@param load_session_id? string Existing ACP session identifier to load.
---@return louiselm.session.Session
function M.new(owner, id, agent_name, definition, options, ready_callback, load_session_id)
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  local working_dir = options.cwd or vim.fn.getcwd()
  local session = setmetatable({
    state = {
      id = id,
      name = options.name or id,
      source = load_session_id == nil and "new" or "loaded",
      agent = agent_name,
      status = "starting",
      working_dir = working_dir,
      current_turn = 0,
      config_options = {},
      skills_policy = definition.skills.policy,
    },
    emitter = Events.new(),
    owner = owner,
    definition = definition,
    options = options,
    load_session_id = load_session_id,
    permission_policy = options.permission_policy or Permission.policy(),
    permission_store = options.permission_store or Permission.store(),
    ready_callback = ready_callback,
    ready_callback_called = false,
    turn_done_turn = nil,
  }, Session)
  if options.on_event ~= nil then
    session:on(options.on_event)
  end
  return session
end

---Start ACP initialization and creation or loading of the agent-side session.
---@param self louiselm.session.Session
---@return boolean started
---@return string? error_message Immediate connection or request error.
function Session:start()
  local client, connect_error = Acp.connect(self.definition, {
    cwd = self.state.working_dir,
    on_notification = function(message)
      handle_notification(self, message)
    end,
    on_request = function(request, respond)
      handle_request(self, request, respond)
    end,
    on_error = function(message)
      fail(self, "ACP transport error: " .. message)
    end,
    on_exit = function(result)
      handle_exit(self, result)
    end,
  })
  if client == nil then
    fail(self, connect_error or "could not connect to ACP agent")
    return false, connect_error
  end
  self.client = client
  local request_id, request_error = client:initialize(nil, function(result, rpc_error)
    handle_initialized(self, result, rpc_error)
  end)
  if request_id == nil then
    fail(self, request_error or "ACP initialize request could not be sent")
    return false, request_error
  end
  return true
end

---Subscribe to typed events from this session.
---@param self louiselm.session.Session
---@param callback louiselm.session.EventCallback Event callback.
---@return fun() unsubscribe Idempotent listener removal function.
function Session:on(callback)
  return self.emitter:on(callback)
end

---Return a snapshot of inspectable session state.
---@param self louiselm.session.Session
---@return louiselm.session.State state Copy of current state.
function Session:inspect()
  local state = copy(self.state)
  state.acp_session_id = self.acp_session_id
  ---@cast state louiselm.session.State
  return state
end

---Set the user-facing name without changing the ACP session.
---@param self louiselm.session.Session
---@param name string New non-empty session name.
---@return boolean renamed
---@return string? error_message
function Session:set_name(name)
  if self.state.status == "disposed" then
    return false, "session is disposed"
  end
  if type(name) ~= "string" or name == "" then
    return false, "session name must be a non-empty string"
  end
  self.state.name = name
  emit(self, "state_changed", { status = self.state.status, activity = self.state.activity })
  return true
end

---Submit one prompt and receive its completion callback.
---@param self louiselm.session.Session
---@param prompt louiselm.session.Prompt Text or ACP prompt content table.
---@param callback? fun(result: unknown, error?: string) Called once on completion or failure.
---@return string|number? request_id ACP request identifier.
---@return string? error_message Validation or state error.
function Session:prompt(prompt, callback)
  if self.state.status ~= "ready" then
    return nil, "session is not ready"
  end
  if type(prompt) == "string" then
    prompt = { { type = "text", text = prompt } }
  elseif type(prompt) ~= "table" then
    return nil, "prompt must be a string or table"
  end
  local client = self.client
  if client == nil then
    return nil, "session has no ACP client"
  end
  set_status(self, "prompting")
  self.state.current_turn = self.state.current_turn + 1
  self.turn_done_turn = nil
  self.prompt_callback = callback
  local request_id, request_error = client:prompt(
    { sessionId = self.acp_session_id, prompt = prompt },
    function(result, rpc_error)
      handle_prompt_result(self, result, rpc_error)
    end
  )
  if request_id == nil then
    local message = request_error or "ACP prompt request could not be sent"
    fail(self, message)
    return nil, message
  end
  return request_id
end

---Request cancellation of the active prompt turn.
---@param self louiselm.session.Session
---@return boolean sent
---@return string? error_message ACP write or state error.
function Session:cancel()
  if
    self.state.status ~= "prompting"
    and self.state.status ~= "waiting_permission"
    and self.state.status ~= "cancelling"
  then
    return false, "session is not prompting"
  end
  local client = self.client
  if client == nil then
    return false, "session has no ACP client"
  end
  local sent, send_error = client:cancel({ sessionId = self.acp_session_id })
  if not sent then
    local message = send_error or "ACP cancel request could not be sent"
    fail(self, message)
    return false, message
  end
  set_status(self, "cancelling")
  return true
end

---Change a supported option while the session is idle and replace all local options from the response.
---@param self louiselm.session.Session
---@param id string Option identifier.
---@param value string|boolean New value matching the advertised option kind.
---@param callback? fun(options: louiselm.session.ConfigOption[]?, error?: string) Called once with the complete replacement list.
---@return string|number? request_id ACP request identifier.
---@return string? error_message Validation, lifecycle, or write error.
function Session:set_config_option(id, value, callback)
  if self.state.status ~= "ready" then
    return nil, "session is not idle"
  end
  if type(id) ~= "string" or id == "" then
    return nil, "config option id must be a non-empty string"
  end
  local option = Validation.find_option(self.state.config_options, id)
  if option == nil then
    return nil, "unknown config option '" .. id .. "'"
  end
  if option.type == "boolean" then
    if type(value) ~= "boolean" then
      return nil, "boolean config option requires a boolean value"
    end
  else
    if type(value) ~= "string" then
      return nil, "select config option requires a string value"
    end
    local available = false
    for _, item in ipairs(option.options or {}) do
      if item.value == value then
        available = true
        break
      end
    end
    if not available then
      return nil, "config option value is not available"
    end
  end
  local client = self.client
  if client == nil then
    return nil, "session has no ACP client"
  end
  local params = { sessionId = self.acp_session_id, configId = id, value = value }
  if option.type == "boolean" then
    params.type = "boolean"
  end
  local previous_model = Validation.model_value(self.state.config_options)
  set_status(self, "configuring")
  local request_id, request_error = client:set_config_option(params, function(result, rpc_error)
    if self.state.status == "disposed" then
      return
    end
    if rpc_error ~= nil then
      local message = "ACP session/set_config_option failed: " .. error_message(rpc_error)
      set_status(self, "ready")
      if callback ~= nil then
        callback(nil, message)
      end
      return
    end
    local options, options_error = Validation.config_options(type(result) == "table" and result.configOptions)
    if options == nil then
      local message = "ACP session/set_config_option returned malformed configOptions: "
        .. (options_error or "invalid options")
      fail(self, message)
      if callback ~= nil then
        callback(nil, message)
      end
      return
    end
    self.state.config_options = options
    if self.state.context ~= nil and previous_model ~= Validation.model_value(options) then
      self.state.context.stale = true
    end
    set_status(self, "ready")
    emit(self, "config_options_changed", copy(options))
    if callback ~= nil then
      callback(copy(options))
    end
  end)
  if request_id == nil then
    set_status(self, "ready")
    return nil, request_error or "ACP config option request could not be sent"
  end
  return request_id
end

---Dispose this session, terminate its ACP process, and remove it from its registry.
---@param self louiselm.session.Session
---@return boolean disposed
---@return string? error_message Process close error, if any.
function Session:dispose()
  if self.state.status == "disposed" then
    return true
  end
  set_status(self, "disposed")
  self.permission_store:clear_session(self.state.id)
  self.prompt_callback = nil
  local client = self.client
  local closed, close_error = true, nil
  if client ~= nil then
    closed, close_error = client:close()
  end
  self.emitter:clear()
  self.owner:remove_session(self.state.id)
  return closed, close_error
end

return M
