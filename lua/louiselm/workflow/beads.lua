---Sequential contained workers over an operator-selected exact Bead list.
local Executor = require("louiselm.workflow.executor")
local Run = require("louiselm.workflow.run")
local Service = require("louiselm.workflow.service")
local Ledger = require("louiselm.workflow.ledger")
local Session = require("louiselm.session")
local Launch = require("louiselm.acp.launch")
---@diagnostic disable-next-line: undefined-global -- Neovim runtime API.
local nvim = vim
local M = {}
local Controller = {}
Controller.__index = Controller

---@class louiselm.workflow.BeadPreparation
---@field grant table Closed Rust GrantRequest; the broker validates all authority fields.
---@field prompt string Worker instruction for this Bead.

---@class louiselm.workflow.BeadResult
---@field bead_id string
---@field session? louiselm.session.Session Retained for verification and promotion.
---@field launch_request louiselm.acp.LaunchRequest
---@field envelope_digest string Run approval digest, never a launch digest.
---@field request_digest string Exact child launch digest.
---@field error? string Worker failure; no retry is made.

---@class louiselm.workflow.BeadExecutorOptions
---@field envelope table Closed Rust RunEnvelope explicitly selected by the operator; start authorizes it.
---@field bead_ids string[] Execution order, restricted to envelope.bead_scope.issue_ids.
---@field agent_id string Installed registered Codex Agent identity.
---@field prepare fun(bead_id: string, callback: fun(prepared: louiselm.workflow.BeadPreparation?, error_message?: string)): boolean, string? Stage the next snapshot and return its exact child grant asynchronously.
---@field on_worker fun(result: louiselm.workflow.BeadResult, continue: fun(proceed: boolean, error_message?: string)) Verification/promotion boundary. Only true starts the next Bead.
---@field session_api? louiselm.session.Api Testable Session boundary; owned Sessions always join this Run.
---@field system? fun(command: string[], options: table, callback: fun(result: table)): unknown Testable subprocess boundary.

---@class louiselm.workflow.BeadExecutor
---@field options louiselm.workflow.BeadExecutorOptions
---@field run louiselm.workflow.Run
---@field executor louiselm.workflow.WorkflowExecutor
---@field status "new"|"active"|"completed"|"failed"|"disposed"
---@field index integer
---@field envelope_digest? string
---@field callback? fun(ok: boolean, error_message?: string)
---@field owned_api? louiselm.session.Api
---@field start fun(self: louiselm.workflow.BeadExecutor, callback: fun(ok: boolean, error_message?: string)): boolean, string?
---@field dispose fun(self: louiselm.workflow.BeadExecutor): boolean, string?

local function digest(value)
  return type(value) == "string" and #value == 71 and value:match("^sha256:[0-9a-f]+$") ~= nil
end

local function strings(value)
  if type(value) ~= "table" or not nvim.islist(value) or #value == 0 then
    return false
  end
  local seen = {}
  for _, id in ipairs(value) do
    if type(id) ~= "string" or id == "" or seen[id] then
      return false
    end
    seen[id] = true
  end
  return true
end

---@param self louiselm.workflow.BeadExecutor
---@param ok boolean
---@param err? string
local function finish(self, ok, err)
  local callback = self.callback
  self.callback = nil
  if self.status ~= "disposed" then
    self.status = ok and "completed" or "failed"
  end
  if callback then
    nvim.schedule(function()
      callback(ok, err)
    end)
  end
end

-- Every boundary is one-shot and scheduled, including caller-provided preparation
-- and verification. Late results cannot advance a disposed or parked Run.
---@param self louiselm.workflow.BeadExecutor
---@param callback function
---@return function
local function response(self, callback)
  local used = false
  return function(first, second)
    if used then
      return
    end
    used = true
    nvim.schedule(function()
      if self.status ~= "active" then
        return
      end
      if self.run.status ~= "active" then
        finish(self, false, "Bead Run is " .. self.run.status)
        return
      end
      callback(first, second)
    end)
  end
end

---@param self louiselm.workflow.BeadExecutor
---@param request table
---@param callback fun(receipt: table)
local function authorize(self, request, callback)
  local encoded, bytes = pcall(nvim.json.encode, request)
  if not encoded then
    return finish(self, false, "authorization request is not JSON: " .. tostring(bytes))
  end
  local done = response(self, function(result)
    if result.code ~= 0 then
      return finish(self, false, "Control broker refused Run authorization")
    end
    local decoded, value = pcall(nvim.json.decode, result.stdout)
    if not decoded or type(value) ~= "table" or value.kind ~= request.kind or type(value.receipt) ~= "table" then
      return finish(self, false, "Control broker returned invalid authorization")
    end
    callback(value.receipt)
  end)
  local started, err = pcall(
    self.options.system or nvim.system,
    { "louiselm-control", "run", "authorize", "--json" },
    { stdin = bytes, text = true, cwd = "/" },
    done
  )
  if not started then
    finish(self, false, "could not start Control broker authorization: " .. tostring(err))
  end
end

---@param self louiselm.workflow.BeadExecutor
---@param outcome string
---@param callback fun()
local function advance(self, outcome, callback)
  local started, err = self.executor:advance(
    outcome,
    nil,
    response(self, function(result, message)
      if not result then
        return finish(self, false, message)
      end
      callback()
    end)
  )
  if not started then
    finish(self, false, err)
  end
end

---@param self louiselm.workflow.BeadExecutor
---@param id string
---@param prepared unknown
---@return string? launch_bytes
---@return string? error_message
local function check_prepared(self, id, prepared)
  if
    type(prepared) ~= "table"
    or type(prepared.grant) ~= "table"
    or type(prepared.prompt) ~= "string"
    or prepared.prompt == ""
  then
    return nil, "worker preparation requires a grant and prompt"
  end
  local grant, envelope = prepared.grant, self.options.envelope
  local bytes, err = Launch.encode(grant.request)
  if not bytes then
    return nil, err
  end
  local request = grant.request
  if
    request.run_id ~= envelope.run_id
    or request.envelope_id ~= envelope.envelope_id
    or request.envelope_revision ~= envelope.envelope_revision
    or request.agent_id ~= self.options.agent_id
  then
    return nil, "worker launch does not match its Run and Agent"
  end
  local scope = grant.beads_mutations
  if type(scope) ~= "table" or not nvim.deep_equal(scope.issue_ids, { id }) or scope.role ~= "worker" then
    return nil, "worker grant must name only its assigned Bead with the worker role"
  end
  return bytes
end

---@param self louiselm.workflow.BeadExecutor
local function next_bead(self)
  local id = self.options.bead_ids[self.index]
  if not id then
    return finish(self, true)
  end
  local prepared_callback = response(self, function(prepared, preparation_error)
    if not prepared then
      return finish(self, false, preparation_error or "worker preparation failed")
    end
    local bytes, err = check_prepared(self, id, prepared)
    if not bytes then
      return finish(self, false, err)
    end
    -- Own the exact bytes sent for approval even if a preparer reuses its table.
    prepared = nvim.deepcopy(prepared)
    local launch = prepared.grant.request
    authorize(
      self,
      { kind = "session", grant = prepared.grant, expected_envelope_digest = self.envelope_digest },
      function(receipt)
        if
          receipt.schema ~= "louiselm.broker.child-authorization/1"
          or receipt.run_id ~= launch.run_id
          or receipt.session_id ~= launch.session_id
          or receipt.authorization_id ~= launch.authorization_id
          or receipt.envelope_revision ~= launch.envelope_revision
          or receipt.envelope_digest ~= self.envelope_digest
          or receipt.request_digest ~= "sha256:" .. nvim.fn.sha256(bytes)
        then
          return finish(self, false, "broker returned a mismatched child authorization")
        end
        local result = {
          bead_id = id,
          launch_request = launch,
          envelope_digest = receipt.envelope_digest,
          request_digest = receipt.request_digest,
        }
        local completed = response(self, function(_, worker_error)
          result.error = worker_error
          advance(self, "verify", function()
            self.options.on_worker(
              result,
              response(self, function(proceed, verification_error)
                if proceed ~= true then
                  return finish(self, false, verification_error or "verification stopped the Run")
                end
                advance(self, "next", function()
                  self.index = self.index + 1
                  next_bead(self)
                end)
              end)
            )
          end)
        end)
        local session, start_error = self.run:create_session(
          self.options.agent_id,
          {
            cwd = "/var/lib/louiselm/sessions/" .. launch.session_id .. "/workspace",
            broker_session_id = launch.session_id,
            launch_request = launch,
            permission_policy = {
              name = "contained-worker",
              evaluate = function()
                return "allow"
              end,
            },
          },
          response(self, function(ready, ready_error)
            if not ready then
              return completed(nil, ready_error or "worker startup failed")
            end
            result.session = ready
            local function prompt()
              local sent, prompt_error = ready:prompt(prepared.prompt, completed)
              if not sent then
                completed(nil, prompt_error or "worker prompt refused")
              end
            end
            if self.index ~= 1 then
              return prompt()
            end
            -- Capture's current Run projection has one anchor Session. The broker
            -- separately retains every child binding; the finite graph generates no work.
            local state = ready:inspect()
            local attached = response(self, function(ok, attach_error)
              if not ok then
                return finish(self, false, attach_error or "Run attachment failed")
              end
              prompt()
            end)
            local attached_started, attach_error = Service.attach({
              id = launch.run_id,
              session_id = state.agent .. "/" .. state.acp_session_id,
              agent = state.agent,
              acp_session_id = state.acp_session_id,
              cwd = state.working_dir,
              load_session = ready.client ~= nil and ready.client.agent_capabilities.loadSession == true,
            }, attached, self.options.system)
            if not attached_started then
              attached(false, attach_error)
            end
          end)
        )
        if not session then
          completed(nil, start_error or "worker startup failed")
        end
      end
    )
  end)
  local started, err = self.options.prepare(id, prepared_callback)
  if not started then
    prepared_callback(nil, err or "could not prepare worker")
  end
end

---Construct a finite workflow; no processes start before start().
---The caller owns snapshot preparation and the verification continuation.
---@param options louiselm.workflow.BeadExecutorOptions
---@return louiselm.workflow.BeadExecutor? controller
---@return string? error_message Invalid definition or off-list Bead.
function M.new(options)
  if type(options) ~= "table" then
    return nil, "Bead executor options must be a table"
  end
  for key in pairs(options) do
    if
      not nvim.tbl_contains(
        { "envelope", "bead_ids", "agent_id", "prepare", "on_worker", "session_api", "system" },
        key
      )
    then
      return nil, "unknown Bead executor option: " .. tostring(key)
    end
  end
  local envelope = options.envelope
  if
    type(envelope) ~= "table"
    or type(envelope.bead_scope) ~= "table"
    or not strings(envelope.bead_scope.issue_ids)
    or type(envelope.run_id) ~= "string"
    or envelope.run_id == ""
  then
    return nil, "Run envelope requires an identity and approved Bead list"
  end
  if not strings(options.bead_ids) then
    return nil, "Bead list must be a non-empty dense array of unique IDs"
  end
  for _, id in ipairs(options.bead_ids) do
    if not nvim.tbl_contains(envelope.bead_scope.issue_ids, id) then
      return nil, "Bead is outside the approved list: " .. id
    end
  end
  if
    type(options.agent_id) ~= "string"
    or options.agent_id == ""
    or type(options.prepare) ~= "function"
    or type(options.on_worker) ~= "function"
  then
    return nil, "Bead executor requires an Agent, prepare and on_worker callbacks"
  end
  if options.system ~= nil and type(options.system) ~= "function" then
    return nil, "system must be a function"
  end
  if
    options.session_api ~= nil
    and (type(options.session_api) ~= "table" or type(options.session_api.create_session) ~= "function")
  then
    return nil, "session_api must support create_session"
  end
  local owned = nvim.tbl_extend(
    "force",
    {},
    options,
    { envelope = nvim.deepcopy(envelope), bead_ids = nvim.deepcopy(options.bead_ids) }
  )
  local manifest = {}
  for index in ipairs(owned.bead_ids) do
    manifest["work_" .. index] = { workflow = "beads", outcomes = { { name = "verify", to = "verify_" .. index } } }
    manifest["verify_" .. index] = {
      workflow = "beads",
      outcomes = {
        index == #owned.bead_ids and { name = "next", terminal = true }
          or { name = "next", to = "work_" .. (index + 1) },
      },
    }
  end
  manifest.work_1.entry = true
  manifest.work_1["generated-work"] = { max = #owned.bead_ids }
  manifest.work_1["park-expiry"] = "1h"
  local executor, validation_error = Executor.new("beads", manifest)
  if not executor then
    return nil, "invalid Bead workflow: " .. nvim.inspect(validation_error)
  end
  local run, run_error = Run.new({ id = envelope.run_id, session_api = options.session_api })
  if not run then
    return nil, run_error
  end
  local controller =
    setmetatable({ options = owned, run = run, executor = executor, index = 1, status = "new" }, Controller)
  return controller
end

---Authorize this exact Run, admit its ledger, then execute one worker at a time.
---Failures and disposal settle callback once on the main loop; no retries occur.
---@param callback fun(ok: boolean, error_message?: string)
---@return boolean started
---@return string? error_message Immediate misuse; asynchronous failures reach callback.
function Controller:start(callback)
  if self.status ~= "new" then
    return false, "Bead Run already started or disposed"
  end
  if type(callback) ~= "function" then
    return false, "Bead Run requires a completion callback"
  end
  self.status, self.callback = "active", callback
  if self.run.session_api == nil then
    local api, errors = Session.new({
      [self.options.agent_id] = {
        command = "/usr/local/lib/louiselm/current/bin/louiselm-launch",
        args = {},
        provider = "openai",
        skills = { policy = "native" },
      },
    })
    if not api then
      finish(self, false, "could not construct worker Session API: " .. nvim.inspect(errors))
      return true
    end
    self.run.session_api, self.owned_api = api, api
  end
  authorize(self, { kind = "run", envelope = self.options.envelope }, function(receipt)
    if
      receipt.schema ~= "louiselm.broker.run-authorization/1"
      or receipt.run_id ~= self.options.envelope.run_id
      or receipt.envelope_revision ~= self.options.envelope.envelope_revision
      or not digest(receipt.envelope_digest)
    then
      return finish(self, false, "broker returned a mismatched Run authorization")
    end
    self.envelope_digest = receipt.envelope_digest
    local admitted = response(self, function(token, err)
      if not token then
        return finish(self, false, err or "Run admission failed")
      end
      local ledger, ledger_error = Ledger.new({ id = self.options.envelope.run_id, token = token }, {
        capture = "louiselm-capture",
        system = self.options.system,
      })
      if not ledger then
        return finish(self, false, ledger_error)
      end
      self.executor.ledger = ledger
      next_bead(self)
    end)
    local started, err = Service.admit(
      { id = self.options.envelope.run_id, generated_work_max = #self.options.bead_ids, park_ttl_ms = 3600000 },
      admitted,
      self.options.system
    )
    if not started then
      admitted(nil, err)
    end
  end)
  return true
end

---Cancel and detach every owned Session. The installed supervisor proves cleanup
---asynchronously; a successful return confirms local disposal, not its receipt.
---@return boolean disposed
---@return string? error_message First local cancellation/disposal error.
function Controller:dispose()
  if self.status == "disposed" then
    return true
  end
  self.status = "disposed"
  local first_error
  for _, worker in ipairs(self.run.workers) do
    if worker:inspect().status == "running" or worker:inspect().status == "prompting" then
      local cancelled, err = worker:cancel()
      if not cancelled then
        first_error = first_error or err or "worker cancellation failed"
      end
    end
  end
  local disposed, err = self.run:dispose()
  if not disposed then
    first_error = first_error or err
  end
  if self.owned_api then
    local closed, api_error = self.owned_api:dispose()
    if not closed then
      first_error = first_error or api_error
    end
  end
  self.executor:cancel()
  finish(self, false, "Bead Run disposed")
  return first_error == nil, first_error
end

return M
