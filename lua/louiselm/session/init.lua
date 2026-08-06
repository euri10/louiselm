local Api = require("louiselm.session.api")

---@class louiselm.session.Module
---@field new fun(definitions: unknown): louiselm.session.Api?, louiselm.agent.ConfigError[] Create a headless session API.

local M = {}

---Create a headless multi-session API for named ACP agents.
---@param definitions unknown Named agent definitions.
---@return louiselm.session.Api? api
---@return louiselm.agent.ConfigError[] errors
function M.new(definitions)
  return Api.new(definitions)
end

return M
