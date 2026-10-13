local Config = require("louiselm.config")
local Health = require("louiselm.health")
local Schema = require("louiselm.schema")
local CaptureCommand = require("louiselm.capture.command")
local Command = require("louiselm.ui.chat.command")
local Keymaps = require("louiselm.ui.keymaps")
local Mcp = require("louiselm.mcp")

---@alias louiselm.SessionControlPhase "starting"|"configuring"|"admitting"|"running"|"cancelling"|"completed"|"failed"|"cancelled"|"timed_out"

---@class louiselm.SessionControlRequest
---@field action "start"|"inspect"|"cancel"
---@field id string Caller-chosen key, at most 128 bytes, unique for this editor registration lifetime.
---@field agent? string Configured Agent; required for start.
---@field cwd? string Absolute working directory; required for start.
---@field name? string Child display name, at most 256 bytes on one line.
---@field prompt? string Explicit non-empty task text; required for start, at most 65536 bytes.
---@field context? {name: string, text: string}[] Up to 16 explicit text snapshots; prompt and context together at most 262144 bytes. Never consumes Chat drafts.
---@field options? {id: string, value: string|boolean}[] Up to 32 ordered advertised choices; Model before dependent effort.
---@field parent_turn_id? string Actual parent admission/turn identity; correlation does not grant authority.
---@field timeout_ms? integer Whole-operation deadline, default 300000, maximum 3600000; expiry disposes the child.
---@field result? boolean Include the bounded assistant-text tail on inspect only.

---@class louiselm.SessionControlSnapshot
---@field id string Operation key, not a Session identity.
---@field revision integer Monotonic change counter for this operation's compact state.
---@field phase louiselm.SessionControlPhase
---@field session_id? string Local child identity.
---@field acp_session_id? string Persistent Agent-side child identity.
---@field turn_id? string Stable admission/turn identity; not proof of dispatch.
---@field status? louiselm.session.Status Latest observed child state.
---@field current_turn? integer Latest observed local attempt ordinal.
---@field admission? string Admission settlement, including explicit uncertainty.
---@field options table<string, string|boolean> Confirmed current option values, not an advertised-choice catalog.
---@field buffer? integer Child transcript buffer, available in the Session switcher.
---@field error? string Failure description, at most 1024 bytes.
---@field stop_reason? string Peer's completed-turn stop reason.
---@field result? {text: string, truncated: boolean} Explicitly requested assistant-text tail, at most 8192 bytes.

---@class louiselm.Module
---@field setup fun(config?: louiselm.Config): boolean, louiselm.schema.Report? Validate configuration and register the plugin's owners and commands.
---@field session_control fun(request: louiselm.SessionControlRequest): louiselm.SessionControlSnapshot?, string? Start, inspect or cancel an operation through the configured owner.
local M = {}

---Create, inspect or cancel a scoped child through this editor's configured owner.
---Call on the main loop; asynchronous work is observed through compact snapshots.
---Exact start retries reuse their operation. Unknown keys after restart are not replayed.
---@param request louiselm.SessionControlRequest Data-only start, inspect or cancel request.
---@return louiselm.SessionControlSnapshot? snapshot State, not proof of task acceptance.
---@return string? error_message Invalid request, missing owner or unknown/conflicting operation.
function M.session_control(request)
  return Command.session_control(request)
end

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

  local errors = Schema.validate(schema, config)
  -- Check references even when another section is malformed; report each
  -- structural field once and never hide an error by applying defaults first.
  if type(config) == "table" then
    local seen = {}
    for _, err in ipairs(errors) do
      seen[err.path] = true
    end
    local _, mcp_errors = Mcp.normalize(config.mcp, config.agents)
    for _, err in ipairs(mcp_errors) do
      if not seen[err.path] then
        errors[#errors + 1] = { type = "validation_failed", path = err.path, message = err.message }
        seen[err.path] = true
      end
    end
  end
  local report = Schema.report(errors)
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
