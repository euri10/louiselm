local Config = require("louiselm.agent.config")
local Health = require("louiselm.agent.health")
local Spawn = require("louiselm.agent.spawn")

local M = {}

---Normalize named agent definitions without mutating the input.
---@param definitions unknown Agent definitions keyed by name.
---@param default_skills_policy? unknown Global Agent Skills policy inherited by agents without an override.
---@return louiselm.agent.Definitions? normalized Normalized definitions, or nil on errors.
---@return louiselm.agent.ConfigError[] errors Every validation error.
function M.normalize(definitions, default_skills_policy)
  return Config.normalize(definitions, default_skills_policy)
end

---Start a normalized agent definition as an external process.
---@param definition louiselm.agent.Definition Normalized agent definition.
---@param on_exit? fun(result: louiselm.agent.ProcessResult) Called once when the process exits.
---@return userdata? handle The process handle, or nil on failure.
---@return string? error_message A validation or launch error.
function M.start(definition, on_exit)
  return Spawn.start(definition, on_exit)
end

---Check an agent executable on PATH and detect its version asynchronously.
---@param definition louiselm.agent.Definition Agent definition to check.
---@param on_result? fun(result: louiselm.agent.HealthResult) Called once with the health result.
---@return userdata? handle The version-check process handle, or nil when no process starts.
---@return string? error_message Validation or immediate availability error.
function M.check(definition, on_result)
  return Health.check(definition, on_result)
end

return M
