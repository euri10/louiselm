local Api = require("louiselm.session.api")

---@class louiselm.session.Module
---@field new fun(definitions: unknown, default_skills_policy?: unknown, options?: louiselm.session.ApiOptions): louiselm.session.Api?, louiselm.agent.ConfigError[] Create a headless session API.
---@field exit_verdict fun(): louiselm.session.ExitVerdict[] Inspect live Sessions across every headless API.
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
  return Api.new(definitions, default_skills_policy, options)
end

---Return a process-wide snapshot of live Sessions relevant to editor exit.
---@return louiselm.session.ExitVerdict[] verdict
function M.exit_verdict()
  return Registry.exit_verdict()
end

---Dispose every live Session in this Neovim process.
---@return boolean disposed
---@return string? error_message First disposal failure, if any.
function M.dispose_all()
  return Registry.dispose_all()
end

return M
