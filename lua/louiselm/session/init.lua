---@class louiselm.session.Api
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
---@field collect_forensics fun(self: louiselm.session.Api, agent_name: string, acp_session_id: string, options?: louiselm.session.ForensicsOptions, callback?: louiselm.session.ForensicsCallback): boolean, string?
---@field dispose fun(self: louiselm.session.Api): boolean, string?
---@field flush_recording fun(self: louiselm.session.Api, callback: louiselm.session.RecordingCallback) Retry/acknowledge queued facts, including final observations after Disposal.

---@class louiselm.session.ApiOptions
---@field permission_store? louiselm.permission.Store Explicit remembered-permission store.
---@field forensics_directory? string Override the private Session Forensics directory.
---@field usage_directory? string Absolute private directory for durable turn recording; defaults to usage/ in the shared LouiseLM state directory.

---@class louiselm.session.ForensicsOptions
---@field diagnosing_session_id? string Durable identity of the diagnosing Session.

---@alias louiselm.session.ForensicsCallback fun(path: string?, error_message: string?)

---@class louiselm.session.Module
---@field new fun(definitions: unknown, default_skills_policy?: unknown, options?: louiselm.session.ApiOptions): louiselm.session.Api?, louiselm.agent.ConfigError[] Create a headless session API.
---@field exit_verdict fun(): louiselm.session.ExitVerdict[] Inspect live Sessions across every headless API.
---@field identity fun(acp_session_id: string): string?, string? Resolve the calling Session's `<agent>/<acp session id>` identity.
---@field collect_forensics fun(agent_name: string, acp_session_id: string, options?: louiselm.session.ForensicsOptions, callback?: louiselm.session.ForensicsCallback): boolean, string? Collect Forensics for any live Session in this process.
---@field dispose_all fun(): boolean, string? Dispose every live Session in this Neovim process.

local M = {}
local Registry = require("louiselm.session.registry")

---Create a headless multi-session API for named ACP agents.
---@param definitions unknown Named agent definitions.
---@param default_skills_policy? unknown Global Agent Skills policy inherited by agents without an override.
---@param options? louiselm.session.ApiOptions Headless owner options.
---@return louiselm.session.Api? api
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions, default_skills_policy, options)
  return Registry.new(definitions, default_skills_policy, options)
end

---Return a process-wide snapshot of live Sessions relevant to editor exit.
---@return louiselm.session.ExitVerdict[] verdict
function M.exit_verdict()
  return Registry.exit_verdict()
end

---Resolve the durable identity of the live Session a caller is running inside.
---
---Pass the ACP session id the calling interaction already exposes about itself.
---The result is independent of which chat view has focus, so it stays correct
---while concurrent Sessions run. Use it verbatim wherever a Session must be
---attributed; an unknown or ambiguous caller id is an error to resolve with the
---maintainer, never a reason to name another Session.
---@param acp_session_id string ACP session id exposed by the calling interaction.
---@return string? session_id Agent-scoped identity, `<agent>/<acp session id>`.
---@return string? error_message Why no single live Session answered for this caller.
function M.identity(acp_session_id)
  return Registry.identity(acp_session_id)
end

---Collect Forensics for any live Session in this Neovim process.
---
---Use this when the diagnosing caller holds no headless API for the subject —
---notably when the chat UI that owns it is the thing being diagnosed. The
---subject is named by Agent plus ACP session id; unrelated Sessions are never
---inspected.
---@param agent_name string Configured Agent name of the subject Session.
---@param acp_session_id string Agent-side ACP session id of the subject Session.
---@param options? louiselm.session.ForensicsOptions Collection options.
---@param callback? louiselm.session.ForensicsCallback Completion boundary for the written path.
---@return boolean started
---@return string? error_message Why no live Session could be diagnosed.
function M.collect_forensics(agent_name, acp_session_id, options, callback)
  return Registry.collect_forensics(agent_name, acp_session_id, options, callback)
end

---Dispose every live Session in this Neovim process.
---@return boolean disposed
---@return string? error_message First disposal failure, if any.
function M.dispose_all()
  return Registry.dispose_all()
end

return M
