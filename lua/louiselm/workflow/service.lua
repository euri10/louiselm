---Asynchronous local-service boundary for durable workflow state.

local M = {}

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.ParkRecord
---@field id string
---@field session_id string
---@field agent string Configured Agent name.
---@field acp_session_id string Agent-side ACP Session identifier.
---@field cwd string Working directory used for cold resume.
---@field load_session boolean Admission result for ACP `session/load` support.
---@field claims string[]
---@field expires_at_ms integer

---@param record louiselm.workflow.ParkRecord
---@param callback fun(ok: boolean, error_message?: string)
---@param system? fun(command: string[], options: table, callback: fun(result: table)): unknown
---@return boolean started
---@return string? error_message
function M.park(record, callback, system)
  if type(record) ~= "table" or type(record.id) ~= "string" or record.id == "" then
    return false, "Park record id must be a non-empty string"
  end
  if type(record.session_id) ~= "string" or record.session_id == "" then
    return false, "Park record session_id must be a non-empty string"
  end
  if type(record.agent) ~= "string" or record.agent == "" then
    return false, "Park record agent must be a non-empty string"
  end
  if type(record.acp_session_id) ~= "string" or record.acp_session_id == "" then
    return false, "Park record acp_session_id must be a non-empty string"
  end
  if type(record.cwd) ~= "string" or record.cwd == "" then
    return false, "Park record cwd must be a non-empty string"
  end
  if record.load_session ~= true then
    return false, "Park requires an Agent that supports session/load"
  end
  if type(record.claims) ~= "table" or #record.claims == 0 then
    return false, "Park record claims must be a non-empty array"
  end
  if type(record.expires_at_ms) ~= "number" or record.expires_at_ms < 1 or record.expires_at_ms % 1 ~= 0 then
    return false, "Park record expires_at_ms must be a positive integer"
  end
  system = system or nvim.system
  local command = {
    "louiselm-capture",
    "run",
    "park",
    "--id",
    record.id,
    "--session-id",
    record.session_id,
    "--agent",
    record.agent,
    "--acp-session-id",
    record.acp_session_id,
    "--cwd",
    record.cwd,
    "--load-session",
    "true",
    "--claims",
    table.concat(record.claims, ","),
    "--expires-at-ms",
    tostring(record.expires_at_ms),
  }
  local started = pcall(system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code == 0 then
        callback(true)
      else
        callback(false, result.stderr ~= "" and result.stderr or "could not persist cold Park")
      end
    end)
  end)
  if not started then
    return false, "could not start Park service command"
  end
  return true
end

return M
