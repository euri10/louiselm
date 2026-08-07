local Deprecate = require("louiselm.schema.deprecate")
local Health = require("louiselm.health")
local Schema = require("louiselm.schema")

local M = {}

---@param message string
---@param level integer
local function notify(message, level)
  -- Setup owns user-facing diagnostics; schema modules remain presentation-free.
  ---@diagnostic disable-next-line: undefined-global
  vim.notify(message, level)
end

---Validate configuration and allow startup only when it is valid.
---@param config unknown User configuration to validate.
---@param schema louiselm.schema.Schema Normalized schema returned by `Schema.define`.
---@return boolean ok False and a report when validation fails; true when startup may continue.
---@return louiselm.schema.Report? report Full validation report on failure.
function M.setup(config, schema)
  Health.reset()
  if type(schema) ~= "table" or schema.type ~= "table" or type(schema.fields) ~= "table" then
    error("setup requires a normalized schema")
  end

  local report = Schema.report(Schema.validate(schema, config))
  if not report.ok then
    -- Neovim provides the severity constants used by its notification API.
    ---@diagnostic disable-next-line: undefined-global
    local error_level = vim.log.levels.ERROR
    notify(report.text, error_level)
    return false, report
  end

  for _, warning in ipairs(Deprecate.find(schema, config)) do
    -- Neovim provides the severity constants used by its notification API.
    ---@diagnostic disable-next-line: undefined-global
    local warning_level = vim.log.levels.WARN
    notify(warning.message, warning_level)
  end
  local registered, registration_error = Health.configure(config, schema)
  if not registered then
    error(registration_error)
  end
  return true
end

return M
