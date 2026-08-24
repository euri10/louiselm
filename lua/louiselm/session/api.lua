local Registry = require("louiselm.session.registry")

---@class louiselm.session.Api
---@field registry louiselm.session.Registry Session owner.
---@field create_session fun(self: louiselm.session.Api, agent_name: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field load_session fun(self: louiselm.session.Api, agent_name: string, acp_session_id: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field discover_sessions fun(self: louiselm.session.Api, options: louiselm.session.DiscoveryOptions?, callback: louiselm.session.DiscoveryCallback): boolean, string?
---@field get_session fun(self: louiselm.session.Api, id: string): louiselm.session.Session?
---@field list_sessions fun(self: louiselm.session.Api): string[]
---@field inspect_agent_limits fun(self: louiselm.session.Api, agent_name: string): louiselm.session.LimitsState?, string?
---@field refresh_agent_limits fun(self: louiselm.session.Api, agent_name: string, callback: fun(state: louiselm.session.LimitsState, error?: string)): boolean, string?
---@field on_agent_limits fun(self: louiselm.session.Api, callback: fun(state: louiselm.session.LimitsState)): fun()?, string?
---@field list_permissions fun(self: louiselm.session.Api): louiselm.permission.Rule[]?, string?
---@field revoke_permission fun(self: louiselm.session.Api, id: string): boolean, string?
---@field dispose fun(self: louiselm.session.Api): boolean, string?

---@class louiselm.session.ApiOptions
---@field permission_store? louiselm.permission.Store Explicit remembered-permission store.

local M = {}
local Api = {}
Api.__index = Api

---Create the headless session API for named agent definitions.
---@param definitions unknown Named agent definitions.
---@param default_skills_policy? unknown Global Agent Skills policy inherited by agents without an override.
---@param options? louiselm.session.ApiOptions Headless owner options.
---@return louiselm.session.Api? api
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions, default_skills_policy, options)
  local registry, errors = Registry.new(definitions, default_skills_policy, options)
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

---Load and asynchronously initialize an existing ACP session.
---@param self louiselm.session.Api
---@param agent_name string Named configured agent.
---@param acp_session_id string Agent-side session identifier to load.
---@param options? louiselm.session.Options Working directory and initial listener.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Called once when loading completes.
---@return louiselm.session.Session? session Loaded session, or nil on immediate failure.
---@return string? error_message Validation or immediate startup error.
function Api:load_session(agent_name, acp_session_id, options, ready_callback)
  return self.registry:load_session(agent_name, acp_session_id, options, ready_callback)
end

---Discover recoverable sessions from every configured ACP agent.
---@param self louiselm.session.Api
---@param options louiselm.session.DiscoveryOptions? Optional exact workspace filter.
---@param callback louiselm.session.DiscoveryCallback Called once with validated sessions and per-agent failures.
---@return boolean started
---@return string? error_message Validation or immediate startup error.
function Api:discover_sessions(options, callback)
  return self.registry:discover_sessions(options, callback)
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

---Return the current Agent-level account-limit state without starting or refreshing an Agent.
---@param self louiselm.session.Api
---@param agent_name string Configured Agent name.
---@return louiselm.session.LimitsState? state
---@return string? error_message Validation failure.
function Api:inspect_agent_limits(agent_name)
  return self.registry:inspect_agent_limits(agent_name)
end

---Refresh account limits through one live capability-advertising Session for an Agent.
---@param self louiselm.session.Api
---@param agent_name string Configured Agent name.
---@param callback fun(state: louiselm.session.LimitsState, error?: string) Completion callback.
---@return boolean started
---@return string? error_message Validation or immediate transport failure.
function Api:refresh_agent_limits(agent_name, callback)
  return self.registry:refresh_agent_limits(agent_name, callback)
end

---Subscribe to Agent-level account-limit changes without attaching to Session telemetry.
---@param self louiselm.session.Api
---@param callback fun(state: louiselm.session.LimitsState) Observer; may run in an ACP fast-event callback.
---@return fun()? unsubscribe
---@return string? error_message Validation or lifecycle failure.
function Api:on_agent_limits(callback)
  return self.registry:on_agent_limits(callback)
end

---List remembered permission rules for inspection.
---@param self louiselm.session.Api
---@return louiselm.permission.Rule[]? rules
---@return string? error_message State read or validation failure.
function Api:list_permissions()
  return self.registry:list_permissions()
end

---Revoke one remembered permission rule.
---@param self louiselm.session.Api
---@param id string Stable rule identifier.
---@return boolean revoked
---@return string? error_message Validation or persistence failure.
function Api:revoke_permission(id)
  return self.registry:revoke_permission(id)
end

---Dispose every session and close the headless API.
---@param self louiselm.session.Api
---@return boolean disposed
---@return string? error_message First close error, if any.
function Api:dispose()
  return self.registry:dispose()
end

return M
