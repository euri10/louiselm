local Agent = require("louiselm.agent")
local Permission = require("louiselm.permission")
local Lifecycle = require("louiselm.session.lifecycle")

---@class louiselm.session.Registry
---@field definitions louiselm.agent.Definitions Normalized named definitions.
---@field sessions table<string, louiselm.session.Session> Live sessions by local id.
---@field order string[] Session ids in creation order.
---@field next_id integer Next local session number.
---@field disposed boolean Whether this registry is closed.
---@field create_session fun(self: louiselm.session.Registry, agent_name: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field load_session fun(self: louiselm.session.Registry, agent_name: string, acp_session_id: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
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
  return setmetatable({ definitions = normalized, sessions = {}, order = {}, next_id = 1, disposed = false }, Registry),
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
