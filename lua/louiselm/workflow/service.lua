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

---@class louiselm.workflow.ParkSummary
---@field id string
---@field agent string
---@field acp_session_id string
---@field cwd string
---@field state "cold_parked"
---@field expires_at_ms integer
---@field generated_work { ceiling: integer, consumed: integer, reserved: integer }

---@class louiselm.workflow.RunAdmissionRecord
---@field id string
---@field session_id string
---@field agent string
---@field acp_session_id string
---@field cwd string
---@field load_session boolean
---@field generated_work_max integer

---Persist one admitted Run before any stage can generate work.
---@param record louiselm.workflow.RunAdmissionRecord
---@param callback fun(ok: boolean, error_message?: string)
---@param system? fun(command: string[], options: table, callback: fun(result: table)): unknown
---@return boolean started
---@return string? error_message
function M.admit(record, callback, system)
  if type(record) ~= "table" or type(record.id) ~= "string" or record.id == "" then
    return false, "Run admission id must be a non-empty string"
  end
  if type(record.session_id) ~= "string" or record.session_id == "" then
    return false, "Run admission session_id must be a non-empty string"
  end
  if type(record.agent) ~= "string" or record.agent == "" then
    return false, "Run admission agent must be a non-empty string"
  end
  if type(record.acp_session_id) ~= "string" or record.acp_session_id == "" then
    return false, "Run admission acp_session_id must be a non-empty string"
  end
  if type(record.cwd) ~= "string" or record.cwd == "" then
    return false, "Run admission cwd must be a non-empty string"
  end
  if type(record.load_session) ~= "boolean" then
    return false, "Run admission load_session must be a boolean"
  end
  local maximum = record.generated_work_max
  if type(maximum) ~= "number" or maximum < 1 or maximum % 1 ~= 0 then
    return false, "Run admission generated_work_max must be a positive integer"
  end
  system = system or nvim.system
  local command = {
    "louiselm-capture",
    "run",
    "admit",
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
    tostring(record.load_session),
    "--generated-work-max",
    tostring(maximum),
  }
  local started = pcall(system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code == 0 then
        callback(true)
      else
        callback(false, result.stderr ~= "" and result.stderr or "could not admit Run")
      end
    end)
  end)
  if not started then
    return false, "could not start Run admission command"
  end
  return true
end

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

---@param callback fun(runs: louiselm.workflow.ParkSummary[], error_message?: string)
---@param system? fun(command: string[], options: table, callback: fun(result: table)): unknown
---@return boolean started
---@return string? error_message
function M.list(callback, system)
  if type(callback) ~= "function" then
    return false, "Park list callback must be a function"
  end
  system = system or nvim.system
  local started = pcall(system, { "louiselm-capture", "run", "list" }, { text = true }, function(result)
    nvim.schedule(function()
      if result.code ~= 0 then
        callback({}, result.stderr ~= "" and result.stderr or "could not list cold Parks")
        return
      end
      local ok, decoded = pcall(nvim.json.decode, result.stdout)
      if not ok or type(decoded) ~= "table" then
        callback({}, "cold Park service returned invalid data")
        return
      end
      local runs = {}
      for _, run in ipairs(decoded) do
        if type(run) ~= "table" or run.state ~= "cold_parked" then
          callback({}, "cold Park service returned malformed data")
          return
        end
        if
          type(run.id) ~= "string"
          or type(run.agent) ~= "string"
          or type(run.acp_session_id) ~= "string"
          or type(run.working_dir) ~= "string"
          or type(run.park_expires_at_ms) ~= "number"
          or type(run.generated_work) ~= "table"
          or type(run.generated_work.ceiling) ~= "number"
          or type(run.generated_work.consumed) ~= "number"
          or type(run.generated_work.reserved) ~= "number"
        then
          callback({}, "cold Park service returned malformed data")
          return
        end
        runs[#runs + 1] = {
          id = run.id,
          agent = run.agent,
          acp_session_id = run.acp_session_id,
          cwd = run.working_dir,
          state = run.state,
          expires_at_ms = run.park_expires_at_ms,
          generated_work = {
            ceiling = run.generated_work.ceiling,
            consumed = run.generated_work.consumed,
            reserved = run.generated_work.reserved,
          },
        }
      end
      callback(runs)
    end)
  end)
  if not started then
    return false, "could not start Park list service command"
  end
  return true
end

return M
