---Asynchronous local-service boundary for durable workflow state.

local M = {}
local Compatibility = require("louiselm.capture.compatibility")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.AgentEnvironmentRecord
---@field id string Run UUID.
---@field token string Generate capability.
---@field database string Canonical Beads database path.

---Build the reserved process environment that routes Agent issue creation through the broker.
---@param record louiselm.workflow.AgentEnvironmentRecord
---@param paths? { shim?: string, br?: string, capture?: string, path?: string } Testable executable paths.
---@return table<string, string>? environment
---@return string? error_message
function M.agent_environment(record, paths)
  if type(record) ~= "table" or type(record.id) ~= "string" or record.id == "" then
    return nil, "Agent environment Run id must be a non-empty string"
  end
  if type(record.token) ~= "string" or record.token == "" then
    return nil, "Agent environment token must be a non-empty string"
  end
  if type(record.database) ~= "string" or record.database == "" then
    return nil, "Agent environment database must be a non-empty string"
  end
  paths = paths or {}
  local shim = paths.shim or nvim.api.nvim_get_runtime_file("scripts/run-tools/br", false)[1]
  local br = paths.br or nvim.fn.exepath("br")
  local capture = paths.capture or nvim.fn.exepath("louiselm-capture")
  local inherited_path = paths.path or nvim.env.PATH or ""
  if
    type(shim) ~= "string"
    or shim == ""
    or type(br) ~= "string"
    or br == ""
    or type(capture) ~= "string"
    or capture == ""
  then
    return nil, "Run tools require the br shim, br, and louiselm-capture executables"
  end
  return {
    LOUISELM_RUN_ID = record.id,
    LOUISELM_RUN_TOKEN = record.token,
    LOUISELM_REAL_BR = br,
    LOUISELM_CAPTURE = capture,
    BEADS_DB = record.database,
    PATH = nvim.fs.dirname(shim) .. (inherited_path == "" and "" or ":" .. inherited_path),
  }
end

---@class louiselm.workflow.ParkRecord
---@field id string
---@field session_id string
---@field agent string Configured Agent name.
---@field acp_session_id string Agent-side ACP Session identifier.
---@field cwd string Working directory used for cold resume.
---@field load_session boolean Admission result for ACP `session/load` support.
---@field claims string[]

---@class louiselm.workflow.ParkSummary
---@field id string
---@field agent string
---@field acp_session_id string
---@field cwd string
---@field state "cold_parked"
---@field parked_at_ms integer Time when the current Park began.
---@field expires_at_ms integer
---@field generated_work { ceiling: integer, consumed: integer, reserved: integer }
---@field claims string[] Beads claims held by the durable Run.

---@class louiselm.workflow.RunAdmissionRecord
---@field id string
---@field generated_work_max integer
---@field park_ttl_ms integer
---@field prepared? { registration_id: string, revision: integer, selection_sha256: string } Exact owner-selected snapshot; never an Agent capability.

---@class louiselm.workflow.RunSessionRecord
---@field id string
---@field session_id string
---@field agent string
---@field acp_session_id string
---@field cwd string
---@field load_session boolean

---Validate an optional exact owner-selected registration without side effects.
---@param prepared unknown
---@return boolean valid
---@return string? error_message
function M.validate_prepared(prepared)
  if prepared == nil then
    return true
  end
  if
    type(prepared) ~= "table"
    or type(prepared.registration_id) ~= "string"
    or #prepared.registration_id ~= 32
    or prepared.registration_id == string.rep("0", 32)
    or prepared.registration_id:match("^[0-9a-f]+$") == nil
    or type(prepared.revision) ~= "number"
    or prepared.revision < 1
    or prepared.revision > 9007199254740991
    or prepared.revision % 1 ~= 0
    or type(prepared.selection_sha256) ~= "string"
    or #prepared.selection_sha256 ~= 64
    or prepared.selection_sha256:match("^[0-9a-f]+$") == nil
  then
    return false, "prepared Run identity is invalid"
  end
  for key in pairs(prepared) do
    if not nvim.tbl_contains({ "registration_id", "revision", "selection_sha256" }, key) then
      return false, "prepared Run identity is invalid"
    end
  end
  return true
end

---Persist one admitted Run before any stage can generate work.
---@param record louiselm.workflow.RunAdmissionRecord
---@param callback fun(token: string?, error_message?: string)
---@param system? fun(command: string[], options: table, callback: fun(result: table)): unknown
---@return boolean started
---@return string? error_message
function M.admit(record, callback, system)
  if type(record) ~= "table" or type(record.id) ~= "string" or record.id == "" then
    return false, "Run admission id must be a non-empty string"
  end
  local maximum = record.generated_work_max
  if type(maximum) ~= "number" or maximum < 1 or maximum % 1 ~= 0 then
    return false, "Run admission generated_work_max must be a positive integer"
  end
  local park_ttl = record.park_ttl_ms
  if type(park_ttl) ~= "number" or park_ttl < 1 or park_ttl % 1 ~= 0 then
    return false, "Run admission park_ttl_ms must be a positive integer"
  end
  local prepared = record.prepared
  local valid, validation_error = M.validate_prepared(prepared)
  if not valid then
    return false, validation_error
  end
  if prepared ~= nil then
    prepared = nvim.deepcopy(prepared)
  end
  local requested_id = record.id
  system = system or nvim.system
  local command = prepared
      and {
        "louiselm-capture",
        "--require-interface=1",
        "phone-control",
        "admit-prepared",
        "--registration-id",
        prepared.registration_id,
        "--revision",
        tostring(prepared.revision),
        "--selection-sha256",
        prepared.selection_sha256,
        "--generated-work-max",
        tostring(maximum),
        "--park-ttl-ms",
        tostring(park_ttl),
      }
    or {
      "louiselm-capture",
      "--require-interface=1",
      "run",
      "admit",
      "--id",
      record.id,
      "--generated-work-max",
      tostring(maximum),
      "--park-ttl-ms",
      tostring(park_ttl),
    }
  local started, spawn_error = pcall(system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code == 0 then
        local ok, response = pcall(nvim.json.decode, result.stdout)
        if
          ok
          and type(response) == "table"
          and type(response.token) == "string"
          and response.token ~= ""
          and (
            prepared == nil
            or (
              response.v == 1
              and response.type == "prepared_admitted"
              and response.run_id == requested_id
              and response.registration_id == prepared.registration_id
              and response.revision == prepared.revision
            )
          )
        then
          callback(response.token)
        else
          callback(nil, "Run admission service returned invalid data")
        end
      else
        callback(nil, Compatibility.command_error(result.stderr ~= "" and result.stderr or "could not admit Run"))
      end
    end)
  end)
  if not started then
    return false, "could not start Run admission command: " .. tostring(spawn_error)
  end
  return true
end

---Attach one initialized ACP Session to its already-admitted Run.
---@param record louiselm.workflow.RunSessionRecord
---@param callback fun(ok: boolean, error_message?: string)
---@param system? fun(command: string[], options: table, callback: fun(result: table)): unknown
---@return boolean started
---@return string? error_message
function M.attach(record, callback, system)
  if type(record) ~= "table" or type(record.id) ~= "string" or record.id == "" then
    return false, "Run attachment id must be a non-empty string"
  end
  for _, field in ipairs({ "session_id", "agent", "acp_session_id", "cwd" }) do
    if type(record[field]) ~= "string" or record[field] == "" then
      return false, "Run attachment " .. field .. " must be a non-empty string"
    end
  end
  if type(record.load_session) ~= "boolean" then
    return false, "Run attachment load_session must be a boolean"
  end
  system = system or nvim.system
  local command = {
    "louiselm-capture",
    "--require-interface=1",
    "run",
    "attach",
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
  }
  local started = pcall(system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code == 0 then
        callback(true)
      else
        callback(
          false,
          Compatibility.command_error(result.stderr ~= "" and result.stderr or "could not attach Run Session")
        )
      end
    end)
  end)
  if not started then
    return false, "could not start Run attachment command"
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
  if type(record.claims) ~= "table" then
    return false, "Park record claims must be an array"
  end
  local claim_count = 0
  for index, claim in ipairs(record.claims) do
    if type(claim) ~= "string" or claim == "" then
      return false, "Park record claims must be an array of non-empty strings"
    end
    claim_count = index
  end
  for key in pairs(record.claims) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > claim_count then
      return false, "Park record claims must be a dense array"
    end
  end
  system = system or nvim.system
  local command = {
    "louiselm-capture",
    "--require-interface=1",
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
  }
  local started = pcall(system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code == 0 then
        callback(true)
      else
        callback(
          false,
          Compatibility.command_error(result.stderr ~= "" and result.stderr or "could not persist cold Park")
        )
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
  local command = { "louiselm-capture", "--require-interface=1", "run", "list" }
  local started = pcall(system, command, { text = true }, function(result)
    nvim.schedule(function()
      if result.code ~= 0 then
        callback({}, Compatibility.command_error(result.stderr ~= "" and result.stderr or "could not list cold Parks"))
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
          or type(run.parked_at_ms) ~= "number"
          or type(run.park_expires_at_ms) ~= "number"
          or type(run.generated_work) ~= "table"
          or type(run.generated_work.ceiling) ~= "number"
          or type(run.generated_work.consumed) ~= "number"
          or type(run.generated_work.reserved) ~= "number"
          or type(run.claims) ~= "table"
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
          parked_at_ms = run.parked_at_ms,
          expires_at_ms = run.park_expires_at_ms,
          generated_work = {
            ceiling = run.generated_work.ceiling,
            consumed = run.generated_work.consumed,
            reserved = run.generated_work.reserved,
          },
          claims = run.claims,
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
