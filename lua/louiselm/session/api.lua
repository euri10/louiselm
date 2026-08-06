local Registry = require("louiselm.session.registry")

---@class louiselm.session.Api
---@field registry louiselm.session.Registry Session owner.
---@field create_session fun(self: louiselm.session.Api, agent_name: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field get_session fun(self: louiselm.session.Api, id: string): louiselm.session.Session?
---@field list_sessions fun(self: louiselm.session.Api): string[]
---@field dispose fun(self: louiselm.session.Api): boolean, string?

local M = {}
local Api = {}
Api.__index = Api

---Create the headless session API for named agent definitions.
---@param definitions unknown Named agent definitions.
---@return louiselm.session.Api? api
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions)
  local registry, errors = Registry.new(definitions)
  if registry == nil then
    return nil, errors
  end
  return setmetatable({ registry = registry }, Api), {}
end

---Create and asynchronously initialize a session for a named agent.
---@param self louiselm.session.Api
---@param agent_name string Named configured agent.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when initialization completes.
---@return louiselm.session.Session? session New session, or nil on immediate failure.
---@return string? error_message Validation or immediate startup error.
function Api:create_session(agent_name, options, ready_callback)
  return self.registry:create_session(agent_name, options, ready_callback)
end

---Look up a live session by local id.
---@param self louiselm.session.Api
---@param id string Local session identifier.
---@return louiselm.session.Session? session
function Api:get_session(id)
  return self.registry:get_session(id)
end

---Return all live local session ids in deterministic order.
---@param self louiselm.session.Api
---@return string[] ids
function Api:list_sessions()
  return self.registry:list_sessions()
end

---Dispose every session and close the headless API.
---@param self louiselm.session.Api
---@return boolean disposed
---@return string? error_message First close error, if any.
function Api:dispose()
  return self.registry:dispose()
end

return M
