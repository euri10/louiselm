local Api = require("louiselm.session.api")

---@class louiselm.session.Module
---@field new fun(definitions: unknown, default_skills_policy?: unknown, options?: louiselm.session.ApiOptions): louiselm.session.Api?, louiselm.agent.ConfigError[] Create a headless session API.

local M = {}

---Create a headless multi-session API for named ACP agents.
---@param definitions unknown Named agent definitions.
---@param default_skills_policy? unknown Global Agent Skills policy inherited by agents without an override.
---@param options? louiselm.session.ApiOptions Headless owner options.
---@return louiselm.session.Api? api
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions, default_skills_policy, options)
  return Api.new(definitions, default_skills_policy, options)
end

return M
