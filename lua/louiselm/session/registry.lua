local Agent = require("louiselm.agent")
local Acp = require("louiselm.acp")
local Permission = require("louiselm.permission")
local Lifecycle = require("louiselm.session.lifecycle")
local Validation = require("louiselm.session.validation")

---@class louiselm.session.DiscoveryOptions
---@field cwd? string Exact ACP workspace filter; omit to discover every workspace.

---@class louiselm.session.DiscoveryError
---@field agent string Configured agent definition name.
---@field message string Sanitized discovery failure.

---@alias louiselm.session.DiscoveryCallback fun(sessions: louiselm.session.DiscoveredSession[], errors: louiselm.session.DiscoveryError[])

---@class louiselm.session.DiscoveryState
---@field id integer
---@field clients table<string, louiselm.acp.Client>
---@field finished table<string, boolean>
---@field remaining integer
---@field sessions louiselm.session.DiscoveredSession[]
---@field errors louiselm.session.DiscoveryError[]
---@field callback louiselm.session.DiscoveryCallback
---@field done boolean

---@class louiselm.session.Registry
---@field definitions louiselm.agent.Definitions Normalized named definitions.
---@field sessions table<string, louiselm.session.Session> Live sessions by local id.
---@field order string[] Session ids in creation order.
---@field next_id integer Next local session number.
---@field next_discovery_id integer Next discovery operation number.
---@field discoveries table<integer, louiselm.session.DiscoveryState> Active adapter discovery operations.
---@field disposed boolean Whether this registry is closed.
---@field create_session fun(self: louiselm.session.Registry, agent_name: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field load_session fun(self: louiselm.session.Registry, agent_name: string, acp_session_id: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field discover_sessions fun(self: louiselm.session.Registry, options: louiselm.session.DiscoveryOptions?, callback: louiselm.session.DiscoveryCallback): boolean, string?
---@field get_session fun(self: louiselm.session.Registry, id: string): louiselm.session.Session?
---@field list_sessions fun(self: louiselm.session.Registry): string[]
---@field dispose fun(self: louiselm.session.Registry): boolean, string?
---@field remove_session fun(self: louiselm.session.Registry, id: string)

local M = {}
local Registry = {}
Registry.__index = Registry

---@param definitions unknown
---@return louiselm.agent.Definitions? normalized
---@return louiselm.agent.ConfigError[] errors
local function normalize_definitions(definitions)
  return Agent.normalize(definitions)
end

---@param value unknown
---@return boolean
local function valid_options(value)
  if value == nil then
    return true
  end
  if type(value) ~= "table" then
    return false
  end
  ---@cast value table
  for key in pairs(value) do
    if key ~= "cwd" and key ~= "name" and key ~= "on_event" and key ~= "permission_policy" then
      return false
    end
  end
  return (value.cwd == nil or type(value.cwd) == "string")
    and (value.name == nil or (type(value.name) == "string" and value.name ~= ""))
end

---Create a registry after strictly normalizing named agent definitions.
---@param definitions unknown Named agent definitions.
---@return louiselm.session.Registry? registry
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions)
  local normalized, errors = normalize_definitions(definitions)
  if normalized == nil then
    return nil, errors
  end
  return setmetatable({
    definitions = normalized,
    sessions = {},
    order = {},
    next_id = 1,
    next_discovery_id = 1,
    discoveries = {},
    disposed = false,
  }, Registry),
    {}
end

---@param load_id string? Existing ACP session id to load.
---@param self louiselm.session.Registry
---@param agent_name string Named configured agent.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when initialization completes.
---@return louiselm.session.Session? session
---@return string? error_message
local function start_session(self, agent_name, options, ready_callback, load_id)
  if self.disposed then
    return nil, "session registry is disposed"
  end
  if type(agent_name) ~= "string" or agent_name == "" then
    return nil, "agent name must be a non-empty string"
  end
  local definition = self.definitions[agent_name]
  if definition == nil then
    return nil, "unknown agent '" .. agent_name .. "'"
  end
  if not valid_options(options) then
    return nil, "session options must contain only a non-empty name, string cwd, and optional on_event callback"
  end
  if options == nil then
    options = {}
  end
  if options.on_event ~= nil and type(options.on_event) ~= "function" then
    return nil, "session option on_event must be a function"
  end

  local permission_policy, policy_error = Permission.policy(options.permission_policy)
  if permission_policy == nil then
    return nil, "invalid session permission policy: " .. (policy_error or "invalid policy")
  end
  local session_options = {
    cwd = options.cwd,
    name = options.name,
    on_event = options.on_event,
    permission_policy = permission_policy,
  }

  local id = "session-" .. self.next_id
  self.next_id = self.next_id + 1
  local session = Lifecycle.new(self, id, agent_name, definition, session_options, ready_callback, load_id)
  self.sessions[id] = session
  self.order[#self.order + 1] = id
  local started, start_error = session:start()
  if not started then
    self.sessions[id] = nil
    table.remove(self.order)
    return nil, start_error
  end
  return session
end

---Create and asynchronously initialize a new session for a named agent.
---@param self louiselm.session.Registry
---@param agent_name string Named configured agent.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when initialization completes.
---@return louiselm.session.Session? session New session, or nil on immediate failure.
---@return string? error_message Validation or immediate startup error.
function Registry:create_session(agent_name, options, ready_callback)
  return start_session(self, agent_name, options, ready_callback, nil)
end

---Load and asynchronously initialize an existing ACP session.
---@param self louiselm.session.Registry
---@param agent_name string Named configured agent.
---@param acp_session_id string Agent-side session identifier to load.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when loading completes.
---@return louiselm.session.Session? session Loaded session, or nil on immediate failure.
---@return string? error_message Validation or immediate startup error.
function Registry:load_session(agent_name, acp_session_id, options, ready_callback)
  if type(acp_session_id) ~= "string" or acp_session_id == "" then
    return nil, "ACP session id must be a non-empty string"
  end
  return start_session(self, agent_name, options, ready_callback, acp_session_id)
end

---@param error_value louiselm.acp.JsonRpcError|string|nil
---@return string
local function error_message(error_value)
  if type(error_value) == "table" then
    return error_value.message
  end
  return tostring(error_value or "ACP request failed")
end

---@param result louiselm.agent.ProcessResult
---@return string
local function exit_message(result)
  if result.signal ~= nil and result.signal ~= 0 then
    return "ACP agent exited with signal " .. tostring(result.signal)
  end
  return "ACP agent exited with code " .. tostring(result.code)
end

---@param sessions louiselm.session.DiscoveredSession[]
local function sort_discovered_sessions(sessions)
  table.sort(sessions, function(left, right)
    local left_updated = left.updated_at or ""
    local right_updated = right.updated_at or ""
    if left_updated ~= right_updated then
      return left_updated > right_updated
    end
    if left.agent ~= right.agent then
      return left.agent < right.agent
    end
    if left.session_id ~= right.session_id then
      return left.session_id < right.session_id
    end
    return left.cwd < right.cwd
  end)
end

---Discover sessions from all configured agents, following each opaque pagination cursor.
---@param self louiselm.session.Registry
---@param options louiselm.session.DiscoveryOptions? Optional exact workspace filter.
---@param callback louiselm.session.DiscoveryCallback Called once with validated sessions and per-agent failures.
---@return boolean started
---@return string? error_message Validation or immediate startup error.
function Registry:discover_sessions(options, callback)
  if self.disposed then
    return false, "session registry is disposed"
  end
  if options ~= nil and type(options) ~= "table" then
    return false, "discovery options must be a table"
  end
  options = options or {}
  for key in pairs(options) do
    if key ~= "cwd" then
      return false, "unknown discovery option '" .. tostring(key) .. "'"
    end
  end
  if options.cwd ~= nil and not Validation.discovery_cwd(options.cwd) then
    return false, "discovery cwd must be an absolute path"
  end
  if type(callback) ~= "function" then
    return false, "discovery callback must be a function"
  end

  local names = {}
  for name in pairs(self.definitions) do
    names[#names + 1] = name
  end
  table.sort(names)

  local discovery = {
    id = self.next_discovery_id,
    clients = {},
    finished = {},
    remaining = #names,
    sessions = {},
    errors = {},
    callback = callback,
    done = false,
  }
  self.next_discovery_id = self.next_discovery_id + 1
  self.discoveries[discovery.id] = discovery

  local function complete()
    if discovery.done or discovery.remaining > 0 then
      return
    end
    discovery.done = true
    self.discoveries[discovery.id] = nil
    sort_discovered_sessions(discovery.sessions)
    table.sort(discovery.errors, function(left, right)
      return left.agent < right.agent
    end)
    discovery.callback(discovery.sessions, discovery.errors)
  end

  for _, name in ipairs(names) do
    local client ---@type louiselm.acp.Client?
    local listed = {}
    local cursors = {}
    local finish ---@type fun(message?: string)

    finish = function(message)
      if discovery.done or discovery.finished[name] then
        return
      end
      discovery.finished[name] = true
      local active_client = discovery.clients[name]
      discovery.clients[name] = nil
      if active_client ~= nil then
        local _, close_error = active_client:close()
        if message == nil and close_error ~= nil then
          message = "could not close ACP discovery agent: " .. close_error
        end
      end
      if message ~= nil then
        discovery.errors[#discovery.errors + 1] = { agent = name, message = message }
      else
        for _, session in ipairs(listed) do
          discovery.sessions[#discovery.sessions + 1] = session
        end
      end
      discovery.remaining = discovery.remaining - 1
      complete()
    end

    local function request_page(cursor)
      if discovery.done or discovery.finished[name] then
        return
      end
      local active_client = client
      if active_client == nil then
        finish("ACP client disappeared during session discovery")
        return
      end
      local params = {}
      if options.cwd ~= nil then
        params.cwd = options.cwd
      end
      if cursor ~= nil then
        params.cursor = cursor
      end
      local request_id, request_error = active_client:list_sessions(params, function(result, rpc_error)
        if discovery.done or discovery.finished[name] then
          return
        end
        if rpc_error ~= nil then
          finish("ACP session/list failed: " .. error_message(rpc_error))
          return
        end
        local page, next_cursor, validation_error = Validation.discovery_page(result, name, options.cwd)
        if page == nil then
          finish("ACP session/list returned malformed data: " .. (validation_error or "invalid response"))
          return
        end
        for _, session in ipairs(page) do
          listed[#listed + 1] = session
        end
        if next_cursor == nil then
          finish()
          return
        end
        if cursors[next_cursor] then
          finish("ACP session/list repeated a pagination cursor")
          return
        end
        cursors[next_cursor] = true
        request_page(next_cursor)
      end)
      if request_id == nil then
        finish(request_error or "ACP session/list request could not be sent")
      end
    end

    local connect_error
    client, connect_error = Acp.connect(self.definitions[name], {
      cwd = options.cwd,
      on_error = function(message)
        finish("ACP transport error: " .. message)
      end,
      on_exit = function(result)
        finish(exit_message(result))
      end,
    })
    if client == nil then
      finish(connect_error or "could not connect to ACP agent")
    elseif discovery.finished[name] then
      client:close()
    else
      discovery.clients[name] = client
      local request_id, request_error = client:initialize(nil, function(_, rpc_error)
        if discovery.done or discovery.finished[name] then
          return
        end
        if rpc_error ~= nil then
          finish("ACP initialize failed: " .. error_message(rpc_error))
          return
        end
        request_page(nil)
      end)
      if request_id == nil then
        finish(request_error or "ACP initialize request could not be sent")
      end
    end
  end
  complete()
  return true
end

---Look up a live session by its local id.
---@param self louiselm.session.Registry
---@param id string Local session identifier.
---@return louiselm.session.Session? session
function Registry:get_session(id)
  return self.sessions[id]
end

---Return live session ids in creation order.
---@param self louiselm.session.Registry
---@return string[] ids
function Registry:list_sessions()
  local ids = {}
  for _, id in ipairs(self.order) do
    if self.sessions[id] ~= nil then
      ids[#ids + 1] = id
    end
  end
  return ids
end

---Dispose every live session and close the registry.
---@param self louiselm.session.Registry
---@return boolean disposed
---@return string? error_message First close error, if any.
function Registry:dispose()
  if self.disposed then
    return true
  end
  self.disposed = true
  local first_error
  for id, discovery in pairs(self.discoveries) do
    discovery.done = true
    for name, client in pairs(discovery.clients) do
      discovery.clients[name] = nil
      local _, close_error = client:close()
      if first_error == nil and close_error ~= nil then
        first_error = close_error
      end
    end
    self.discoveries[id] = nil
  end
  local ids = self:list_sessions()
  for _, id in ipairs(ids) do
    local session = self.sessions[id]
    if session ~= nil then
      local _, close_error = session:dispose()
      if first_error == nil and close_error ~= nil then
        first_error = close_error
      end
    end
  end
  return first_error == nil, first_error
end

---Remove one disposed session from the registry.
---@param self louiselm.session.Registry
---@param id string Local session identifier.
function Registry:remove_session(id)
  self.sessions[id] = nil
  for index, current_id in ipairs(self.order) do
    if current_id == id then
      table.remove(self.order, index)
      return
    end
  end
end

return M
