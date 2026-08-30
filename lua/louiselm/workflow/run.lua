---Runtime ownership and bounded cancellation for one workflow Run.

local M = {}
local Run = {}
Run.__index = Run
local Service = require("louiselm.workflow.service")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

---@class louiselm.workflow.RunWorker
---@field client louiselm.acp.Client? ACP client used for cold-Park admission.
---@field cancel fun(self: louiselm.workflow.RunWorker): boolean, string?
---@field dispose fun(self: louiselm.workflow.RunWorker): boolean, string?
---@field inspect fun(self: louiselm.workflow.RunWorker): table

---@class louiselm.workflow.RunOptions
---@field session_api? louiselm.session.Api API used to create owned Sessions.
---@field cancellation_timeout_ms? integer Maximum time to wait for cooperative acknowledgment.
---@field schedule? fun(delay_ms: integer, callback: fun()) Testable scheduling boundary; defaults to `vim.defer_fn`.
---@field park_record? louiselm.workflow.ParkRecord Durable cold-Park record.
---@field park_service? fun(record: louiselm.workflow.ParkRecord, callback: fun(ok: boolean, error_message?: string)): boolean, string? Async persistence boundary.
---@field claims? string[] Beads claims restored from a durable Run.
---@field generated_work? { ceiling: integer, consumed: integer, reserved: integer } Generated-work accounting restored from a durable Run.

---@class louiselm.workflow.Run
---@field session_api? louiselm.session.Api
---@field cancellation_timeout_ms integer
---@field schedule fun(delay_ms: integer, callback: fun())
---@field workers louiselm.workflow.RunWorker[] Owned Sessions.
---@field status "active"|"cancelling"|"cancelled"|"parked"|"disposed"
---@field cancel fun(self: louiselm.workflow.Run, callback?: fun()): boolean, string?
---@field park fun(self: louiselm.workflow.Run, callback?: fun(ok: boolean, error_message?: string)): boolean, string?
---@field accept_park fun(self: louiselm.workflow.Run): boolean, string?
---@field accept_resume fun(self: louiselm.workflow.Run, claims?: string[], generated_work?: { ceiling: integer, consumed: integer, reserved: integer }): boolean, string?
---@field create_session fun(self: louiselm.workflow.Run, agent_name: string, options?: louiselm.session.Options, ready_callback?: fun(session: louiselm.session.Session?, error?: string)): louiselm.session.Session?, string?
---@field adopt_session fun(self: louiselm.workflow.Run, session: louiselm.session.Session): boolean, string?
---@field dispose fun(self: louiselm.workflow.Run): boolean, string?
---@field emergency_stop fun(self: louiselm.workflow.Run): boolean, string?
---@field park_record? louiselm.workflow.ParkRecord
---@field claims string[] Beads claims owned by this Run.
---@field generated_work? { ceiling: integer, consumed: integer, reserved: integer } Generated-work accounting owned by this Run.
---@field park_service fun(record: louiselm.workflow.ParkRecord, callback: fun(ok: boolean, error_message?: string)): boolean, string?
---@field park_cold fun(self: louiselm.workflow.Run, request: louiselm.workflow.ColdParkRequest, callback?: fun(ok: boolean, error_message?: string)): boolean, string?

---@class louiselm.workflow.ColdParkRequest
---@field id string Durable Run UUID.
---@field claims string[] Beads claims owned by this Run.

---@param value unknown
---@return boolean
local function is_worker(value)
  return type(value) == "table"
    and type(value.cancel) == "function"
    and type(value.dispose) == "function"
    and type(value.inspect) == "function"
end

---@param claims unknown
---@return string[]? copied
---@return string? error_message
local function copy_claims(claims)
  if type(claims) ~= "table" then
    return nil, "Run claims must be an array"
  end
  local copied = {}
  local count = 0
  for index, claim in ipairs(claims) do
    if type(claim) ~= "string" or claim == "" then
      return nil, "Run claims must be an array of non-empty strings"
    end
    copied[index] = claim
    count = index
  end
  for key in pairs(claims) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > count then
      return nil, "Run claims must be a dense array"
    end
  end
  return copied
end

---@param generated_work unknown
---@return { ceiling: integer, consumed: integer, reserved: integer }? copied
---@return string? error_message
local function copy_generated_work(generated_work)
  if type(generated_work) ~= "table" then
    return nil, "Run generated_work must be a table"
  end
  local copied = {}
  for _, field in ipairs({ "ceiling", "consumed", "reserved" }) do
    local value = generated_work[field]
    if type(value) ~= "number" or value < 0 or value % 1 ~= 0 then
      return nil, "Run generated_work fields must be non-negative integers"
    end
    copied[field] = value
  end
  return copied
end

---@param worker louiselm.workflow.RunWorker
---@return boolean
local function acknowledged(worker)
  local state = worker.inspect(worker)
  return state.status ~= "prompting" and state.status ~= "waiting_permission" and state.status ~= "cancelling"
end

---@param self louiselm.workflow.Run
local function transition_parked(self)
  self.status = "parked"
  for _, worker in ipairs(self.workers) do
    if not acknowledged(worker) then
      worker:cancel()
    end
  end
end

---@param worker louiselm.workflow.RunWorker
---@return louiselm.workflow.ParkRecord? record
---@return string? error_message
local function cold_park_record(worker, request)
  local state = worker.inspect(worker)
  local client = worker.client
  local capabilities = client and client.agent_capabilities or nil
  if type(state) ~= "table" or type(client) ~= "table" then
    return nil, "cold Park requires a live ACP Session"
  end
  if
    type(state.agent) ~= "string"
    or state.agent == ""
    or type(state.acp_session_id) ~= "string"
    or state.acp_session_id == ""
  then
    return nil, "cold Park requires an initialized ACP Session"
  end
  if type(state.working_dir) ~= "string" or state.working_dir == "" then
    return nil, "cold Park requires a Session working directory"
  end
  if type(capabilities) ~= "table" or capabilities.loadSession ~= true then
    return nil, "cold Park requires an Agent that supports session/load"
  end
  if type(state.current_turn) ~= "number" or state.current_turn < 1 then
    -- An Agent that has never received a prompt has nothing durable on disk
    -- for `session/load` to find, even when it advertises `loadSession`:
    -- the capability describes what the Agent can resume, not what this
    -- particular Session has persisted yet (louiselm-aaw0.1).
    return nil, "cold Park requires a Session that has sent at least one prompt"
  end
  return {
    id = request.id,
    session_id = state.agent .. "/" .. state.acp_session_id,
    agent = state.agent,
    acp_session_id = state.acp_session_id,
    cwd = state.working_dir,
    load_session = true,
    claims = request.claims,
  }
end

---@param options? louiselm.workflow.RunOptions
---@return louiselm.workflow.Run? run
---@return string? error_message
function M.new(options)
  if options == nil then
    options = {}
  elseif type(options) ~= "table" then
    return nil, "Run options must be a table"
  end
  ---@cast options louiselm.workflow.RunOptions
  local timeout = options.cancellation_timeout_ms or 1000
  if type(timeout) ~= "number" or timeout < 0 or timeout % 1 ~= 0 then
    return nil, "cancellation_timeout_ms must be a non-negative integer"
  end
  local schedule = options.schedule
  if schedule == nil then
    schedule = function(delay_ms, callback)
      nvim.defer_fn(callback, delay_ms)
    end
  elseif type(schedule) ~= "function" then
    return nil, "schedule must be a function"
  end
  local claims = {}
  if options.claims ~= nil then
    local copied, claims_error = copy_claims(options.claims)
    if copied == nil then
      return nil, claims_error
    end
    claims = copied
  end
  local generated_work
  if options.generated_work ~= nil then
    local copied, generated_work_error = copy_generated_work(options.generated_work)
    if copied == nil then
      return nil, generated_work_error
    end
    generated_work = copied
  end
  local run = setmetatable({
    session_api = options.session_api,
    cancellation_timeout_ms = timeout,
    schedule = schedule,
    workers = {},
    status = "active",
    park_record = options.park_record,
    park_service = options.park_service or Service.park,
    claims = claims,
    generated_work = generated_work,
  }, Run)
  return run
end

---Adopt a Session into this Run's supervision tree.
---@param self louiselm.workflow.Run
---@param session louiselm.session.Session
---@return boolean adopted
---@return string? error_message
function Run:adopt_session(session)
  if self.status ~= "active" then
    return false, "Run is no longer active"
  end
  if not is_worker(session) then
    return false, "Run worker must support cancel, dispose, and inspect"
  end
  ---@diagnostic disable-next-line: assign-type-mismatch -- Session is the production RunWorker implementation.
  self.workers[#self.workers + 1] = session
  ---@diagnostic disable-next-line: undefined-field -- Runtime ownership is deliberately attached at construction.
  session.owner_run = self
  return true
end

---Create a Session through the Run's owner API and immediately place it in the tree.
---@param self louiselm.workflow.Run
---@param agent_name string Configured Agent name.
---@param options? louiselm.session.Options Session options.
---@param ready_callback? fun(session: louiselm.session.Session?, error?: string) Startup callback.
---@return louiselm.session.Session? session
---@return string? error_message
function Run:create_session(agent_name, options, ready_callback)
  if self.session_api == nil then
    return nil, "Run has no Session API"
  end
  if self.status ~= "active" then
    return nil, "Run is no longer active"
  end
  local function on_ready(session, error_message)
    if session ~= nil then
      self:adopt_session(session)
    end
    if ready_callback ~= nil then
      ready_callback(session, error_message)
    end
  end
  local session, create_error = self.session_api:create_session(agent_name, options, on_ready)
  if session == nil then
    return nil, create_error
  end
  local already_owned = false
  for _, worker in ipairs(self.workers) do
    if worker == session then
      already_owned = true
      break
    end
  end
  local adopted, adopt_error
  if already_owned then
    adopted = true
  else
    adopted, adopt_error = self:adopt_session(session)
  end
  if not adopted then
    session:dispose()
    return nil, adopt_error
  end
  return session
end

---Cancel every owned Session, then dispose workers that do not acknowledge by the deadline.
---The guarantee covers every worker LouiseLM can address, not every process an ACP Agent may
---spawn; an Agent ignoring both cancel and dispose remains the outer harness's responsibility.
---@param self louiselm.workflow.Run
---@param callback? fun() Called once after all workers acknowledge or are disposed.
---@return boolean started
---@return string? error_message
function Run:cancel(callback)
  if self.status == "disposed" then
    return false, "Run is disposed"
  end
  if self.status == "parked" then
    return false, "Run is parked"
  end
  if self.status == "cancelled" or self.status == "cancelling" then
    return true
  end
  self.status = "cancelling"
  for _, worker in ipairs(self.workers) do
    if not acknowledged(worker) then
      worker:cancel()
    end
  end
  self.schedule(self.cancellation_timeout_ms, function()
    if self.status ~= "cancelling" then
      return
    end
    for _, worker in ipairs(self.workers) do
      if not acknowledged(worker) then
        worker.dispose(worker)
      end
    end
    self.status = "cancelled"
    if callback ~= nil then
      callback()
    end
  end)
  return true
end

---Park this Run without destroying its Sessions.
---Cooperative cancellation is requested for active turns, but no acknowledgment is awaited and no
---worker is disposed. This escape is therefore available even when an ACP Agent is wedged.
---@param self louiselm.workflow.Run
---@param callback? fun(ok: boolean, error_message?: string) Called after the local Park transition.
---@return boolean parked
---@return string? error_message
function Run:park(callback)
  if self.status == "disposed" then
    return false, "Run is disposed"
  end
  if self.status == "parked" then
    return true
  end
  local function complete(ok, error_message)
    if not ok then
      if callback ~= nil then
        callback(false, error_message)
      end
      return
    end
    transition_parked(self)
    if callback ~= nil then
      callback(true)
    end
  end
  if self.park_record ~= nil then
    local started, start_error = self.park_service(self.park_record, complete)
    if not started then
      return false, start_error
    end
    return true
  end
  complete(true)
  return true
end

---Accept an already-durable service-originated Park without persisting it again.
---@param self louiselm.workflow.Run
---@return boolean parked
---@return string? error_message
function Run:accept_park()
  if self.status == "disposed" then
    return false, "Run is disposed"
  end
  if self.status == "parked" then
    return true
  end
  transition_parked(self)
  return true
end

---Accept an already-durable operator resume without mutating the service again.
---@param self louiselm.workflow.Run
---@param claims? string[] Claims restored from the durable Run.
---@param generated_work? { ceiling: integer, consumed: integer, reserved: integer } Generated-work accounting restored from the durable Run.
---@return boolean resumed
---@return string? error_message
function Run:accept_resume(claims, generated_work)
  if self.status == "disposed" then
    return false, "Run is disposed"
  end
  if claims ~= nil then
    local copied, claims_error = copy_claims(claims)
    if copied == nil then
      return false, claims_error
    end
    self.claims = copied
  end
  if generated_work ~= nil then
    local copied, generated_work_error = copy_generated_work(generated_work)
    if copied == nil then
      return false, generated_work_error
    end
    self.generated_work = copied
  end
  if self.status == "active" then
    return true
  end
  if self.status ~= "parked" then
    return false, "only a Parked Run can resume"
  end
  self.status = "active"
  return true
end

---Admit and durably Park a single-Session Run for cold resume.
---@param self louiselm.workflow.Run
---@param request louiselm.workflow.ColdParkRequest
---@param callback? fun(ok: boolean, error_message?: string) Called after persistence completes.
---@return boolean started
---@return string? error_message
function Run:park_cold(request, callback)
  if self.status == "disposed" then
    return false, "Run is disposed"
  end
  if type(request) ~= "table" then
    return false, "cold Park request must be a table"
  end
  if #self.workers ~= 1 then
    return false, "cold Park requires exactly one owned Session"
  end
  local record, record_error = cold_park_record(self.workers[1], request)
  if record == nil then
    return false, record_error
  end
  local previous_record = self.park_record
  self.park_record = record
  local started, start_error = self:park(function(ok, error_message)
    if not ok then
      self.park_record = previous_record
    end
    if callback ~= nil then
      callback(ok, error_message)
    end
  end)
  if not started then
    self.park_record = previous_record
  end
  return started, start_error
end

---Dispose every owned Session and end the Run.
---@param self louiselm.workflow.Run
---@return boolean disposed
---@return string? error_message
function Run:dispose()
  if self.status == "disposed" then
    return true
  end
  self.status = "disposed"
  local first_error
  for _, worker in ipairs(self.workers) do
    local disposed, dispose_error = worker.dispose(worker)
    if not disposed and first_error == nil then
      first_error = dispose_error or "worker disposal failed"
    end
  end
  return first_error == nil, first_error
end

---Destructively stop the Run and dispose every owned Session.
---@param self louiselm.workflow.Run
---@return boolean stopped
---@return string? error_message
function Run:emergency_stop()
  return self:dispose()
end

return M
