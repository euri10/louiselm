local Acp = require("louiselm.acp")
local Permission = require("louiselm.permission")
local Events = require("louiselm.session.events")

---@alias louiselm.session.Status "starting"|"ready"|"prompting"|"cancelling"|"error"|"disposed"
---@alias louiselm.session.Prompt string|table

---@class louiselm.session.State
---@field id string Local session identifier.
---@field agent string Named agent definition.
---@field status louiselm.session.Status Lifecycle state.
---@field working_dir string ACP working directory.
---@field current_turn integer Number of the current or most recently completed turn.

---@class louiselm.session.Options
---@field cwd? string Working directory for the ACP session.
---@field on_event? louiselm.session.EventCallback Initial event listener.
---@field permission_policy? louiselm.permission.Policy Policy for agent-requested operations.

---@class louiselm.session.Session
---@field state louiselm.session.State Internal mutable state.
---@field emitter louiselm.session.EventEmitter Event subscribers.
---@field client louiselm.acp.Client? ACP client.
---@field acp_session_id string? Agent-side session identifier.
---@field prompt_callback? fun(result: unknown, error?: string) Current prompt completion callback.
---@field ready_callback? fun(session: louiselm.session.Session?, error?: string) Session startup callback.
---@field ready_callback_called boolean Whether startup callback ran.
---@field turn_done_turn integer? Turn for which the completion event was emitted.
---@field owner louiselm.session.Registry Registry that owns this session.
---@field definition louiselm.agent.Definition Agent process definition.
---@field options louiselm.session.Options Session options.
---@field permission_policy louiselm.permission.Policy Policy for agent-requested operations.
---@field start fun(self: louiselm.session.Session): boolean, string?
---@field on fun(self: louiselm.session.Session, callback: louiselm.session.EventCallback): fun()
---@field inspect fun(self: louiselm.session.Session): louiselm.session.State
---@field prompt fun(self: louiselm.session.Session, prompt: louiselm.session.Prompt, callback?: fun(result: unknown, error?: string)): string|number?, string?
---@field cancel fun(self: louiselm.session.Session): boolean, string?
---@field dispose fun(self: louiselm.session.Session): boolean, string?

local M = {}
local Session = {}
Session.__index = Session

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
  self.emitter:emit({ type = event_type, session_id = self.state.id, data = data, respond = respond })
end

---@param self louiselm.session.Session
---@param message string
local function fail(self, message)
  if self.state.status == "disposed" or self.state.status == "error" then
    return
  end
  self.state.status = "error"
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
    self.state.status = "ready"
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
      emit(self, "tool_call_finished", update)
    else
      emit(self, "tool_call_started", update)
    end
  elseif update_type == "turn_done" or update_type == "turn_complete" then
    complete_turn(self, update)
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
    fail(self, "malformed ACP permission request")
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
    fail(self, "invalid ACP permission request: " .. (policy_error or "permission policy failed"))
    return
  end
  data.policy_decision = decision
  if decision ~= "ask" then
    local automatic_response = Permission.gates.response(data, decision)
    if automatic_response ~= nil then
      respond(automatic_response)
      return
    end
  end
  emit(self, "permission_requested", data, respond)
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
  local request_id, request_error = client:new_session(
    { cwd = self.state.working_dir, mcpServers = {} },
    function(session_result, session_error)
      if self.state.status == "disposed" then
        return
      end
      if session_error ~= nil then
        fail(self, "ACP session/new failed: " .. error_message(session_error))
        return
      end
      if
        type(session_result) ~= "table"
        or type(session_result.sessionId) ~= "string"
        or session_result.sessionId == ""
      then
        fail(self, "ACP session/new returned a malformed result")
        return
      end
      self.acp_session_id = session_result.sessionId
      self.state.status = "ready"
      if not self.ready_callback_called then
        self.ready_callback_called = true
        local callback = self.ready_callback
        if callback ~= nil then
          callback(self)
        end
      end
    end
  )
  if request_id == nil then
    fail(self, "ACP session/new failed: " .. (request_error or "request could not be sent"))
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
  complete_turn(self, result)
end

---@param owner louiselm.session.Registry
---@param id string
---@param agent_name string
---@param definition louiselm.agent.Definition
---@param options louiselm.session.Options
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string)
---@return louiselm.session.Session
function M.new(owner, id, agent_name, definition, options, ready_callback)
  ---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
  local working_dir = options.cwd or vim.fn.getcwd()
  local session = setmetatable({
    state = {
      id = id,
      agent = agent_name,
      status = "starting",
      working_dir = working_dir,
      current_turn = 0,
    },
    emitter = Events.new(),
    owner = owner,
    definition = definition,
    options = options,
    permission_policy = options.permission_policy or Permission.policy(),
    ready_callback = ready_callback,
    ready_callback_called = false,
    turn_done_turn = nil,
  }, Session)
  if options.on_event ~= nil then
    session:on(options.on_event)
  end
  return session
end

---Start ACP initialization and creation of the agent-side session.
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
  local state = {}
  for key, value in pairs(self.state) do
    state[key] = value
  end
  return state
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
  self.state.status = "prompting"
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
  if self.state.status ~= "prompting" and self.state.status ~= "cancelling" then
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
  self.state.status = "cancelling"
  return true
end

---Dispose this session, terminate its ACP process, and remove it from its registry.
---@param self louiselm.session.Session
---@return boolean disposed
---@return string? error_message Process close error, if any.
function Session:dispose()
  if self.state.status == "disposed" then
    return true
  end
  self.state.status = "disposed"
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
