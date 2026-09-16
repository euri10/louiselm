local Config = require("louiselm.config")
local Health = require("louiselm.health")
local Schema = require("louiselm.schema")
local CaptureCommand = require("louiselm.capture.command")
local Command = require("louiselm.ui.chat.command")
local Keymaps = require("louiselm.ui.keymaps")

local M = {}

---@param message string
---@param level integer
local function notify(message, level)
  -- Setup owns user-facing diagnostics; schema modules remain presentation-free.
  ---@diagnostic disable-next-line: undefined-global
  vim.notify(message, level)
end

---Validate configuration and allow startup only when it is valid.
---The annotated type is `lua/louiselm/types.lua`, generated from the schema by
---`./scripts/generate-luacats`; it drives editor completion here, while the runtime
---still validates the untouched user table and rejects anything the type misses.
---@param config louiselm.Config? User configuration to validate.
---@return boolean ok False and a report when validation fails; true when startup may continue.
---@return louiselm.schema.Report? report Full validation report on failure.
function M.setup(config)
  local schema = Config.schema

  local report = Schema.report(Schema.validate(schema, config))
  if not report.ok then
    -- Neovim provides the severity constants used by its notification API.
    ---@diagnostic disable-next-line: undefined-global
    local error_level = vim.log.levels.ERROR
    notify(report.text, error_level)
    return false, report
  end

  local transition_error
  if #require("louiselm.session").exit_verdict() > 0 then
    transition_error = "live Sessions retain their configuration; close them before reconfiguring LouiseLM"
  else
    local capture_configured, capture_error = CaptureCommand.configure(config)
    if not capture_configured then
      transition_error = capture_error
    end
  end
  if transition_error ~= nil then
    report = Schema.report({ { type = "validation_failed", path = "", message = transition_error } })
    ---@diagnostic disable-next-line: undefined-global -- Setup reports a rejected lifecycle transition.
    notify(report.text, vim.log.levels.ERROR)
    return false, report
  end

  local registered, registration_error = Health.configure(config, schema)
  if not registered then
    error(registration_error)
  end
  local command_configured, command_error = Command.configure(config)
  if not command_configured then
    error(command_error)
  end
  local keymaps_configured, keymaps_error = Keymaps.configure(config)
  if not keymaps_configured then
    error(keymaps_error)
  end
  Command.surface_abandonment()
  Command.register()
  CaptureCommand.register()
  Health.register()
  return true
end

return M
