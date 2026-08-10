local Protocol = require("louiselm.acp.protocol")
local Transport = require("louiselm.acp.transport")

---@class louiselm.acp.ClientOptions
---@field cwd? string Working directory for the agent process.
---@field on_notification? fun(message: louiselm.acp.JsonRpcNotification) Called for agent notifications.
---@field on_request? fun(message: louiselm.acp.JsonRpcRequest, respond: fun(result: unknown, error?: louiselm.acp.JsonRpcError): boolean, string?) Called for agent requests.
---@field on_error? fun(message: string) Called for transport or protocol errors.
---@field on_stderr? fun(message: string) Called for agent stderr chunks.
---@field on_exit? fun(result: louiselm.agent.ProcessResult) Called once after process exit.

---@class louiselm.acp.Client
---@field transport louiselm.acp.Transport
---@field next_id integer
---@field pending table<string|number, fun(result: unknown, error?: louiselm.acp.JsonRpcError)|false>
---@field initialized boolean
---@field agent_capabilities table<string, unknown> Capabilities reported during initialization.
---@field options louiselm.acp.ClientOptions
---@field request fun(self: louiselm.acp.Client, method: string, params?: unknown, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field notify fun(self: louiselm.acp.Client, method: string, params?: unknown): boolean, string?
---@field initialize fun(self: louiselm.acp.Client, params?: table, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field new_session fun(self: louiselm.acp.Client, params: table, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field load_session fun(self: louiselm.acp.Client, params: table, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field list_sessions fun(self: louiselm.acp.Client, params?: table, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field prompt fun(self: louiselm.acp.Client, params: table, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field set_config_option fun(self: louiselm.acp.Client, params: table, callback?: fun(result: unknown, error?: louiselm.acp.JsonRpcError)): string|number?, string?
---@field cancel fun(self: louiselm.acp.Client, params: table): boolean, string?
---@field respond fun(self: louiselm.acp.Client, id: string|number, result: unknown, rpc_error?: louiselm.acp.JsonRpcError): boolean, string?
---@field close fun(self: louiselm.acp.Client): boolean, string?
---@field is_open fun(self: louiselm.acp.Client): boolean

local M = { PROTOCOL_VERSION = 1 }
local Client = {}
Client.__index = Client

---@param client louiselm.acp.Client
---@param message louiselm.acp.JsonRpcMessage
local function receive_message(client, message)
  if message.method ~= nil then
    if message.id ~= nil then
      ---@cast message louiselm.acp.JsonRpcRequest
      local on_request = client.options.on_request
      if on_request ~= nil then
        on_request(message, function(result, rpc_error)
          return client:respond(message.id, result, rpc_error)
        end)
      end
      return
    end
    ---@cast message louiselm.acp.JsonRpcNotification
    local on_notification = client.options.on_notification
    if on_notification ~= nil then
      on_notification(message)
    end
    return
  end

  local callback = client.pending[message.id]
  if callback == nil then
    local on_error = client.options.on_error
    if on_error ~= nil then
      on_error("received response for unknown request id")
    end
    return
  end
  client.pending[message.id] = nil
  if callback == false then
    return
  end
  if message.error ~= nil then
    callback(nil, message.error)
  else
    callback(message.result)
  end
end

---@param client louiselm.acp.Client
---@return true|nil initialized
---@return string? error_message
local function require_initialized(client)
  if not client.initialized then
    return nil, "ACP client is not initialized"
  end
  return true
end

---@param options louiselm.acp.ClientOptions
---@return louiselm.acp.TransportOptions
local function transport_options(options)
  return {
    cwd = options.cwd,
    on_message = function(message)
      -- Assigned by connect before a process can answer a request.
    end,
    on_error = options.on_error,
    on_stderr = options.on_stderr,
    on_exit = options.on_exit,
  }
end

---Connect to an ACP agent process without starting a protocol request.
---@param definition louiselm.agent.Definition Agent command and process configuration.
---@param options? louiselm.acp.ClientOptions Client callbacks and working directory.
---@return louiselm.acp.Client? client
---@return string? error_message Validation or launch error.
function M.connect(definition, options)
  local client = setmetatable({
    next_id = 1,
    pending = {},
    initialized = false,
    agent_capabilities = {},
    options = options or {},
  }, Client)
  local callbacks = transport_options(client.options)
  callbacks.on_message = function(message)
    receive_message(client, message)
  end
  local transport, start_error = Transport.start(definition, callbacks)
  if transport == nil then
    return nil, start_error
  end
  client.transport = transport
  return client
end

---Send a request and invoke its callback exactly once when the response arrives.
---@param self louiselm.acp.Client
---@param method string Method name.
---@param params? unknown Method parameters.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Response callback.
---@return string|number? id Request identifier, or nil on validation/write failure.
---@return string? error_message Validation or write error.
function Client:request(method, params, callback)
  local id = self.next_id
  local request, request_error = Protocol.request(id, method, params)
  if request == nil then
    return nil, request_error
  end
  self.next_id = id + 1
  if callback ~= nil then
    self.pending[id] = callback
  else
    self.pending[id] = false
  end
  local sent, send_error = self.transport:send(request)
  if not sent then
    self.pending[id] = nil
    return nil, send_error
  end
  return id
end

---Send a notification without waiting for a response.
---@param self louiselm.acp.Client
---@param method string Method name.
---@param params? unknown Method parameters.
---@return boolean sent
---@return string? error_message Validation or write error.
function Client:notify(method, params)
  local notification, notification_error = Protocol.notification(method, params)
  if notification == nil then
    return false, notification_error
  end
  return self.transport:send(notification)
end

---Negotiate ACP protocol version and capabilities.
---@param self louiselm.acp.Client
---@param params? table ACP initialize parameters; defaults advertise no optional client features.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Called with the initialize result.
---@return string|number? id Request identifier, or nil on validation/write failure.
---@return string? error_message Validation or write error.
function Client:initialize(params, callback)
  if self.initialized then
    return nil, "ACP client is already initialized"
  end
  if params == nil then
    params = {
      protocolVersion = M.PROTOCOL_VERSION,
      clientCapabilities = {
        fs = { readTextFile = false, writeTextFile = false },
        session = { configOptions = { boolean = {} } },
        terminal = false,
      },
      clientInfo = { name = "louiselm.nvim", version = "0.1.0" },
    }
  end
  return self:request("initialize", params, function(result, rpc_error)
    if rpc_error ~= nil then
      if callback ~= nil then
        callback(result, rpc_error)
      end
      return
    end
    if type(result) ~= "table" or result.protocolVersion ~= M.PROTOCOL_VERSION then
      local error_response = {
        code = -32600,
        message = "agent selected an unsupported ACP protocol version",
      }
      if callback ~= nil then
        callback(nil, error_response)
      end
      return
    end
    self.initialized = true
    self.agent_capabilities = type(result.agentCapabilities) == "table" and result.agentCapabilities or {}
    if callback ~= nil then
      callback(result)
    end
  end)
end

---Create a new ACP session.
---@param self louiselm.acp.Client
---@param params table Session parameters.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Response callback.
---@return string|number? id Request identifier, or nil on failure.
---@return string? error_message Validation or write error.
function Client:new_session(params, callback)
  local initialized, initialization_error = require_initialized(self)
  if not initialized then
    return nil, initialization_error
  end
  return self:request("session/new", params, callback)
end

---Load an existing ACP session.
---@param self louiselm.acp.Client
---@param params table Session parameters.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Response callback.
---@return string|number? id Request identifier, or nil on failure.
---@return string? error_message Validation or write error.
function Client:load_session(params, callback)
  local initialized, initialization_error = require_initialized(self)
  if not initialized then
    return nil, initialization_error
  end
  if self.agent_capabilities.loadSession ~= true then
    return nil, "ACP agent does not support session/load"
  end
  return self:request("session/load", params, callback)
end

---List sessions known to an ACP agent that advertises discovery support.
---@param self louiselm.acp.Client
---@param params? table Optional workspace filter and pagination cursor.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Response callback.
---@return string|number? id Request identifier, or nil on failure.
---@return string? error_message Validation or write error.
function Client:list_sessions(params, callback)
  local initialized, initialization_error = require_initialized(self)
  if not initialized then
    return nil, initialization_error
  end
  local session_capabilities = self.agent_capabilities.sessionCapabilities
  if type(session_capabilities) ~= "table" or type(session_capabilities.list) ~= "table" then
    return nil, "ACP agent does not support session/list"
  end
  return self:request("session/list", params or {}, callback)
end

---Submit a prompt to an ACP session.
---@param self louiselm.acp.Client
---@param params table Prompt parameters.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Response callback.
---@return string|number? id Request identifier, or nil on failure.
---@return string? error_message Validation or write error.
function Client:prompt(params, callback)
  local initialized, initialization_error = require_initialized(self)
  if not initialized then
    return nil, initialization_error
  end
  return self:request("session/prompt", params, callback)
end

---Change one ACP session configuration option.
---@param self louiselm.acp.Client
---@param params table Session id, option id, and typed value.
---@param callback? fun(result: unknown, error?: louiselm.acp.JsonRpcError) Response callback.
---@return string|number? id Request identifier, or nil on failure.
---@return string? error_message Validation or write error.
function Client:set_config_option(params, callback)
  local initialized, initialization_error = require_initialized(self)
  if not initialized then
    return nil, initialization_error
  end
  return self:request("session/set_config_option", params, callback)
end

---Cancel the active prompt turn for an ACP session.
---@param self louiselm.acp.Client
---@param params table Cancellation parameters.
---@return boolean sent
---@return string? error_message Validation or write error.
function Client:cancel(params)
  local initialized, initialization_error = require_initialized(self)
  if not initialized then
    return false, initialization_error
  end
  return self:notify("session/cancel", params)
end

---Respond to an ACP request sent by the agent.
---@param self louiselm.acp.Client
---@param id string|number Request identifier.
---@param result unknown Result value, ignored when error is supplied.
---@param rpc_error? louiselm.acp.JsonRpcError Error response instead of a result.
---@return boolean sent
---@return string? error_message Validation or write error.
function Client:respond(id, result, rpc_error)
  local response
  if rpc_error ~= nil then
    response = Protocol.error_response(id, rpc_error.code, rpc_error.message, rpc_error.data)
  else
    response = Protocol.response(id, result)
  end
  return self.transport:send(response)
end

---Close the ACP connection and terminate its agent process.
---@param self louiselm.acp.Client
---@return boolean closed
---@return string? error_message Process close error.
function Client:close()
  return self.transport:close()
end

---Return whether the ACP process is available for writes.
---@param self louiselm.acp.Client
---@return boolean open
function Client:is_open()
  return self.transport:is_open()
end

return M
