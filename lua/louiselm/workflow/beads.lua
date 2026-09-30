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
local advance, next_bead

---@class louiselm.workflow.BeadPreparation
---@field grant table Closed Rust GrantRequest; the broker validates all authority fields.
---@field prompt string Worker instruction for this Bead.
---@field verification { snapshot: string, snapshot_digest: string, plan: string, plan_digest: string, verifier_grant: table } Controller-owned baseline, plan and separate verifier grant.
---@field base_commit? string Exact source snapshot HEAD for Run promotion.

---@class louiselm.workflow.BeadResult
---@field bead_id string
---@field session? louiselm.session.Session Retained for verification and promotion.
---@field launch_request louiselm.acp.LaunchRequest
---@field envelope_digest string Run approval digest, never a launch digest.
---@field request_digest string Exact child launch digest.
---@field error? string Worker failure; no retry is made.
---@field verification? table Broker-owned durable status, including ordered command results.
---@field verification_passed? boolean True only for complete passing broker evidence.
---@field verifier_session_id? string Distinct verifier's broker Session identity.
---@field worker_turn_id? string Durable turn observation identity, when prompt was admitted.
---@field verification_request_id? string Durable fixed-plan observation identity, when available.
---@field promotion? table Exact accepted Run worktree commit.

---@class louiselm.workflow.BeadFailure
---@field bead_id string
---@field outcome "worker_failed"|"verification_failed"|"rejected"
---@field observation_ids string[] Durable turn or broker identities that locate worker and verifier evidence.
---@field comment_operation_id? string Durable broker mutation identity, when returned.
---@field comment_status "unconfirmed"|"confirmed"
---@field comment_text string Safe comment to copy to a host Beads tracker after a VM Run.

---@class louiselm.workflow.BeadRunSummary
---@field run_id string
---@field accepted string[]
---@field commits table<string, string> Accepted Bead to exact Run branch commit, when promotion is enabled.
---@field branch? string Fetchable Run branch for VM to host handoff.
---@field failed louiselm.workflow.BeadFailure[]
---@field text string Human-readable host handoff, with no worker output.

---@class louiselm.workflow.BeadExecutorOptions
---@field envelope table Closed Rust RunEnvelope explicitly selected by the operator; start authorizes it.
---@field bead_ids string[] Execution order, restricted to envelope.bead_scope.issue_ids.
---@field agent_id string Installed registered Codex Agent identity.
---@field prepare fun(bead_id: string, callback: fun(prepared: louiselm.workflow.BeadPreparation?, error_message?: string), expected_head?: string): boolean, string? Stage the next snapshot from the current Run branch HEAD.
---@field on_worker fun(result: louiselm.workflow.BeadResult, continue: fun(accepted: boolean?, error_message?: string)) Operator acceptance. True accepts passing work, false rejects and continues; nil stops the Run.
---@field worktree? { path: string, journal_parent: string, head: string } Dedicated clean run/<run-id> checkout and initial HEAD.
---@field on_promotion? fun(preview: table, decide: fun(accepted: boolean)) Present the exact preview to the maintainer.
---@field session_api? louiselm.session.Api Testable Session boundary; owned Sessions always join this Run.
---@field system? fun(command: string[], options: table, callback: fun(result: table)): unknown Testable subprocess boundary.

---@class louiselm.workflow.BeadExecutor
---@field options louiselm.workflow.BeadExecutorOptions
---@field run louiselm.workflow.Run
---@field executor louiselm.workflow.WorkflowExecutor
---@field status "new"|"active"|"completed"|"failed"|"disposed"
---@field index integer
---@field envelope_digest? string
---@field head? string Current committed Run branch HEAD.
---@field callback? fun(ok: boolean, error_message?: string)
---@field summary louiselm.workflow.BeadRunSummary
---@field owned_api? louiselm.session.Api
---@field start fun(self: louiselm.workflow.BeadExecutor, callback: fun(ok: boolean, error_message?: string, summary: louiselm.workflow.BeadRunSummary)): boolean, string?
---@field dispose fun(self: louiselm.workflow.BeadExecutor): boolean, string?

local function digest(value)
  return type(value) == "string" and #value == 71 and value:match("^sha256:[0-9a-f]+$") ~= nil
end

local function present(value)
  return value ~= nil and value ~= nvim.NIL
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
    local summary = self.summary
    local lines = {
      "Run " .. summary.run_id,
      "Accepted: " .. (#summary.accepted > 0 and table.concat(summary.accepted, ", ") or "(none)"),
    }
    if summary.branch then
      lines[#lines + 1] = "Branch: " .. summary.branch
    end
    for _, id in ipairs(summary.accepted) do
      if summary.commits[id] then
        lines[#lines + 1] = "Commit " .. id .. ": " .. summary.commits[id]
      end
    end
    for _, failure in ipairs(summary.failed) do
      lines[#lines + 1] = "Failed: "
        .. failure.bead_id
        .. " ("
        .. failure.outcome
        .. "; observations: "
        .. table.concat(failure.observation_ids, ", ")
        .. "; broker comment: "
        .. (failure.comment_operation_id or failure.comment_status)
        .. ")"
      lines[#lines + 1] = "Host comment for " .. failure.bead_id .. ": " .. failure.comment_text
      if failure.comment_status == "unconfirmed" then
        lines[#lines + 1] = "Inspect the VM broker operation and tracker before copying this unconfirmed comment."
      end
    end
    summary.text = table.concat(lines, "\n")
    nvim.schedule(function()
      callback(ok, err, nvim.deepcopy(summary))
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
      return finish(
        self,
        false,
        request.kind == "bead_failure" and "Control broker refused Bead failure comment"
          or "Control broker refused Run authorization"
      )
    end
    local decoded, value = pcall(nvim.json.decode, result.stdout)
    if not decoded or type(value) ~= "table" or value.kind ~= request.kind or type(value.receipt) ~= "table" then
      return finish(self, false, "Control broker returned invalid " .. request.kind .. " response")
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
---@param argv string[]
---@param request? table
---@param expected string
---@param callback fun(value: table?, error_message?: string)
local function control(self, argv, request, expected, callback)
  local bytes
  if request then
    local encoded, value = pcall(nvim.json.encode, request)
    if not encoded then
      callback(nil, "verification request is not JSON")
      return
    end
    bytes = value
  end
  local done = response(self, function(result)
    if type(result) ~= "table" or result.code ~= 0 then
      callback(nil, "Control broker " .. expected .. " unavailable")
      return
    end
    local decoded, value = pcall(nvim.json.decode, result.stdout)
    if not decoded or type(value) ~= "table" or (expected ~= "inspect" and value.kind ~= expected) then
      callback(nil, "Control broker returned invalid " .. expected)
      return
    end
    callback(value)
  end)
  local started, err = pcall(self.options.system or nvim.system, argv, { stdin = bytes, text = true, cwd = "/" }, done)
  if not started then
    done({ code = 1, stderr = tostring(err) })
  end
end

---@param session_id string
---@param operation string
---@return string
local function operation_id(session_id, operation)
  return "verification-" .. operation .. "-" .. nvim.fn.sha256(session_id):sub(1, 32)
end

---@param self louiselm.workflow.BeadExecutor
---@param action string
---@param request table
---@param callback fun(value: table?, error_message?: string)
local function promotion_control(self, action, request, callback)
  local encoded, bytes = pcall(nvim.json.encode, request)
  if not encoded then
    return callback(nil, "promotion request is not JSON")
  end
  local done = response(self, function(result)
    if type(result) ~= "table" or result.code ~= 0 then
      return callback(nil, "Run promotion " .. action .. " refused; inspect its journal and worktree")
    end
    local decoded, value = pcall(nvim.json.decode, result.stdout)
    if not decoded or type(value) ~= "table" then
      return callback(nil, "Run promotion returned invalid " .. action)
    end
    callback(value)
  end)
  local started, err = pcall(
    self.options.system or nvim.system,
    { "louiselm-control", "promotion", action, "--json" },
    { stdin = bytes, text = true, cwd = "/" },
    done
  )
  if not started then
    done({ code = 1, stderr = tostring(err) })
  end
end

---@param self louiselm.workflow.BeadExecutor
---@param result louiselm.workflow.BeadResult
---@param callback fun(accepted: boolean?, error_message?: string)
local function promote(self, result, callback)
  local launch = result.launch_request
  local selection = {
    schema = "louiselm.run-promotion-selection/1",
    run_id = launch.run_id,
    bead_id = result.bead_id,
    producer_session_id = launch.session_id,
    verifier_session_id = result.verifier_session_id,
    request_id = operation_id(launch.session_id, "promotion"),
    checkout = self.options.worktree.path,
    journal_parent = self.options.worktree.journal_parent,
    expected_head = self.head,
  }
  promotion_control(self, "preview", { selection = selection }, function(preview, preview_error)
    if not preview then
      return callback(nil, preview_error)
    end
    if
      preview.schema ~= "louiselm.run-promotion-preview/1"
      or type(preview.changes) ~= "table"
      or not digest(preview.approval_digest)
      or type(preview.selection) ~= "table"
      or preview.selection.run_id ~= selection.run_id
      or preview.selection.bead_id ~= selection.bead_id
      or preview.selection.producer_session_id ~= selection.producer_session_id
      or preview.selection.verifier_session_id ~= selection.verifier_session_id
      or preview.selection.request_id ~= selection.request_id
      or preview.selection.expected_head ~= self.head
    then
      return callback(nil, "Run promotion returned a mismatched preview")
    end
    self.options.on_promotion(
      preview,
      response(self, function(accepted)
        if accepted ~= true then
          return callback(false)
        end
        promotion_control(
          self,
          "commit",
          { selection = preview.selection, approval_digest = preview.approval_digest },
          function(committed, commit_error)
            if not committed then
              return callback(nil, commit_error)
            end
            if
              committed.schema ~= "louiselm.run-promotion-commit/1"
              or committed.bead_id ~= result.bead_id
              or type(committed.commit) ~= "string"
              or not committed.commit:match("^[0-9a-f]+$")
              or (#committed.commit ~= 40 and #committed.commit ~= 64)
            then
              return callback(nil, "Run promotion returned an invalid commit")
            end
            self.head = committed.commit
            result.promotion = committed
            callback(true)
          end
        )
      end)
    )
  end)
end

---@param self louiselm.workflow.BeadExecutor
---@param result louiselm.workflow.BeadResult
local function handoff(self, result)
  self.options.on_worker(
    result,
    response(self, function(accepted, decision_error)
      if accepted == nil then
        return finish(self, false, decision_error or "operator stopped the Run")
      end
      if result.verification and result.verification.state ~= "completed" then
        return finish(self, false, "Run verification outcome is uncertain")
      end
      local function proceed()
        advance(self, "next", function()
          self.index = self.index + 1
          next_bead(self)
        end)
      end
      local function reject()
        local outcome = result.error and "worker_failed"
          or result.verification_passed ~= true and "verification_failed"
          or "rejected"
        local ids = { result.worker_turn_id or result.launch_request.session_id }
        if result.verifier_session_id then
          ids[#ids + 1] = result.verification_request_id or result.verifier_session_id
        end
        local comment_text = "Run "
          .. self.summary.run_id
          .. ": "
          .. outcome
          .. "; observations: "
          .. table.concat(ids, ", ")
        local request_id = "failure-" .. nvim.fn.sha256(self.summary.run_id .. ":" .. result.bead_id):sub(1, 32)
        local failure = {
          bead_id = result.bead_id,
          outcome = outcome,
          observation_ids = ids,
          comment_status = "unconfirmed",
          comment_text = comment_text,
        }
        self.summary.failed[#self.summary.failed + 1] = failure
        authorize(self, {
          kind = "bead_failure",
          report = {
            run_id = self.summary.run_id,
            expected_envelope_digest = self.envelope_digest,
            request_id = request_id,
            bead_id = result.bead_id,
            outcome = outcome,
            observation_ids = ids,
          },
        }, function(receipt)
          if type(receipt.operation_id) == "string" then
            failure.comment_operation_id = receipt.operation_id
          end
          if
            receipt.request_id ~= request_id
            or type(receipt.operation_id) ~= "string"
            or type(receipt.outcome) ~= "table"
            or receipt.outcome.kind ~= "completed"
          then
            return finish(self, false, "broker Bead failure comment is not confirmed")
          end
          failure.comment_status = "confirmed"
          proceed()
        end)
      end
      if accepted == true and not result.error and result.verification_passed == true then
        if self.options.worktree then
          return promote(
            self,
            result,
            response(self, function(promoted, promotion_error)
              if promoted == nil then
                return finish(self, false, promotion_error)
              end
              if promoted == false then
                return reject()
              end
              self.summary.commits[result.bead_id] = result.promotion.commit
              self.summary.accepted[#self.summary.accepted + 1] = result.bead_id
              local disposed, dispose_error = result.session:dispose()
              if not disposed then
                return finish(self, false, dispose_error or "promoted worker disposal failed")
              end
              proceed()
            end)
          )
        end
        self.summary.accepted[#self.summary.accepted + 1] = result.bead_id
        return proceed()
      end
      reject()
    end)
  )
end

---@param self louiselm.workflow.BeadExecutor
---@param result louiselm.workflow.BeadResult
---@param prepared louiselm.workflow.BeadPreparation
---@param input_id string
---@param input_digest string
local function verify_worker(self, result, prepared, input_id, input_digest)
  if result.error then
    return handoff(self, result)
  end
  local launch = result.launch_request
  local grant = prepared.verification.verifier_grant
  local verifier_launch = grant.request
  result.verifier_session_id = verifier_launch.session_id
  local expires = self.options.envelope.expires_at_ms
  local reported = false
  local function report(status, passed, message)
    if reported then
      return
    end
    reported = true
    result.verification = status
    result.verification_passed = passed
    result.error = message
    handoff(self, result)
  end
  local function unknown(message)
    report({ state = "unknown" }, false, message)
  end
  control(
    self,
    { "louiselm-control", "session", "inspect", launch.session_id, "--json" },
    nil,
    "inspect",
    function(status, err)
      if not status or status.state ~= "running" or type(status.broker_head) ~= "table" then
        return unknown(err or "producer has no Running broker receipt")
      end
      local park = {
        schema = "louiselm.launch.lifecycle-request/1",
        protocol_version = 1,
        request_id = operation_id(launch.session_id, "park"),
        authorization_id = operation_id(launch.session_id, "park-authorize"),
        session_id = launch.session_id,
        run_id = launch.run_id,
        action = "park",
        expected_state = "running",
        expected_receipt_sequence = status.broker_head.sequence,
        envelope_revision = launch.envelope_revision,
      }
      control(
        self,
        { "louiselm-control", "verification", "--json" },
        { kind = "park", request = park },
        "parked",
        function(parked, park_error)
          if not parked then
            return unknown(park_error or "producer Park is uncertain")
          end
          local export = {
            schema = "louiselm.launch.verification/1",
            protocol_version = 1,
            request_id = operation_id(launch.session_id, "export"),
            launch = launch,
            head = parked.head,
            expires_at_ms = expires,
            operation = { kind = "export", input_id = input_id, input_digest = input_digest },
          }
          control(
            self,
            { "louiselm-control", "verification", "--json" },
            { kind = "export", request = export },
            "exported",
            function(exported, export_error)
              if not exported then
                return unknown(export_error or "producer export is uncertain")
              end
              authorize(
                self,
                { kind = "session", grant = grant, expected_envelope_digest = self.envelope_digest },
                function(receipt)
                  local bytes = Launch.encode(verifier_launch)
                  if receipt.request_digest ~= "sha256:" .. nvim.fn.sha256(bytes) then
                    return unknown("broker returned a mismatched verifier authorization")
                  end
                  local verifier, start_error = self.run:create_session(
                    verifier_launch.agent_id,
                    {
                      cwd = "/var/lib/louiselm/sessions/" .. verifier_launch.session_id .. "/workspace",
                      broker_session_id = verifier_launch.session_id,
                      launch_request = verifier_launch,
                      permission_policy = {
                        name = "contained-verifier",
                        evaluate = function()
                          return "deny"
                        end,
                      },
                    },
                    response(self, function(ready, ready_error)
                      if not ready then
                        return unknown(ready_error or "verifier startup failed")
                      end
                      control(
                        self,
                        { "louiselm-control", "session", "inspect", verifier_launch.session_id, "--json" },
                        nil,
                        "inspect",
                        function(verifier_status, inspect_error)
                          if
                            not verifier_status
                            or verifier_status.state ~= "running"
                            or type(verifier_status.broker_head) ~= "table"
                            or verifier_status.broker_head.sequence ~= 1
                          then
                            return unknown(inspect_error or "verifier has no fresh Running receipt")
                          end
                          local run_request = {
                            schema = "louiselm.launch.verification/1",
                            protocol_version = 1,
                            request_id = operation_id(verifier_launch.session_id, "run"),
                            launch = verifier_launch,
                            head = verifier_status.broker_head,
                            expires_at_ms = expires,
                            operation = {
                              kind = "run",
                              producer_session_id = launch.session_id,
                              export_request_id = export.request_id,
                              export_digest = exported.export_digest,
                              job_digest = exported.evidence.job.job_digest,
                            },
                          }
                          control(
                            self,
                            { "louiselm-control", "verification", "--json" },
                            { kind = "run", request = run_request },
                            "executed",
                            function(_, run_error)
                              -- A lost reply may follow a spent intent. Read durable status; never resend.
                              control(
                                self,
                                { "louiselm-control", "verification", "--json" },
                                { kind = "status", session_id = verifier_launch.session_id },
                                "status",
                                function(view, status_error)
                                  if not view then
                                    return unknown(status_error or run_error or "verification outcome is unknown")
                                  end
                                  if type(view.status) == "table" and type(view.status.detail) == "table" then
                                    local execution = view.status.detail.execution
                                    if type(execution) == "table" and type(execution.request) == "table" then
                                      result.verification_request_id = execution.request.request_id
                                    end
                                  end
                                  report(view.status, view.commands_passed == true)
                                end
                              )
                            end
                          )
                        end
                      )
                    end)
                  )
                  if not verifier then
                    unknown(start_error or "verifier startup failed")
                  end
                end
              )
            end
          )
        end
      )
    end
  )
end

---@param self louiselm.workflow.BeadExecutor
---@param outcome string
---@param callback fun()
advance = function(self, outcome, callback)
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
  local verification = prepared.verification
  if
    type(verification) ~= "table"
    or type(verification.snapshot) ~= "string"
    or type(verification.plan) ~= "string"
    or not digest(verification.snapshot_digest)
    or verification.plan_digest ~= envelope.verification_plan_digest
    or type(verification.verifier_grant) ~= "table"
  then
    return nil, "verification requires the approved plan, baseline and verifier grant"
  end
  if self.options.worktree and prepared.base_commit ~= self.head then
    return nil, "next Bead snapshot does not match the Run branch HEAD"
  end
  local verifier = verification.verifier_grant
  local verifier_bytes = Launch.encode(verifier.request)
  if
    not verifier_bytes
    or verifier.request.run_id ~= envelope.run_id
    or verifier.request.envelope_id ~= envelope.envelope_id
    or verifier.request.envelope_revision ~= envelope.envelope_revision
    or verifier.request.agent_id ~= self.options.agent_id
    or verifier.request.session_id == request.session_id
    or present(verifier.commands)
    or present(verifier.beads_mutations)
    or present(verifier.dependencies)
    or present(verifier.skill_requests)
    or present(verifier.provider_requests)
  then
    return nil, "verifier must be a distinct grant without ordinary capabilities"
  end
  return bytes
end

---@param self louiselm.workflow.BeadExecutor
next_bead = function(self)
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
    local input_id = "verify-input-" .. launch.session_id
    control(
      self,
      { "louiselm-control", "verification", "--json" },
      {
        kind = "stage",
        input_id = input_id,
        snapshot = prepared.verification.snapshot,
        snapshot_digest = prepared.verification.snapshot_digest,
        expected_base_commit = self.head,
        plan = prepared.verification.plan,
        plan_digest = prepared.verification.plan_digest,
      },
      "staged",
      function(staged, stage_error)
        if not staged then
          return finish(self, false, stage_error or "verification inputs could not be staged")
        end
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
                verify_worker(self, result, prepared, input_id, staged.input_digest)
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
                  else
                    result.worker_turn_id = sent
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
      end
    )
  end)
  local started, err = self.options.prepare(id, prepared_callback, self.head)
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
      not nvim.tbl_contains({
        "envelope",
        "bead_ids",
        "agent_id",
        "prepare",
        "on_worker",
        "worktree",
        "on_promotion",
        "session_api",
        "system",
      }, key)
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
  if type(envelope.max_sessions) ~= "number" or envelope.max_sessions < 2 * #options.bead_ids then
    return nil, "Run envelope needs one worker and one verifier Session per Bead"
  end
  if type(envelope.bead_scope.max_mutations) ~= "number" or envelope.bead_scope.max_mutations < #options.bead_ids then
    return nil, "Run envelope needs one failure-comment mutation per Bead"
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
  if options.worktree ~= nil then
    local worktree = options.worktree
    if
      type(worktree) ~= "table"
      or type(worktree.path) ~= "string"
      or worktree.path == ""
      or type(worktree.journal_parent) ~= "string"
      or worktree.journal_parent == ""
      or type(worktree.head) ~= "string"
      or not worktree.head:match("^[0-9a-f]+$")
      or (#worktree.head ~= 40 and #worktree.head ~= 64)
      or type(options.on_promotion) ~= "function"
    then
      return nil, "Run worktree needs a path, journal, HEAD and promotion decision callback"
    end
  elseif options.on_promotion ~= nil then
    return nil, "promotion decision requires a Run worktree"
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
  local controller = setmetatable({
    options = owned,
    run = run,
    executor = executor,
    index = 1,
    status = "new",
    head = options.worktree and options.worktree.head,
    summary = {
      run_id = envelope.run_id,
      accepted = {},
      commits = {},
      branch = options.worktree and "run/" .. envelope.run_id or nil,
      failed = {},
      text = "",
    },
  }, Controller)
  return controller
end

---Authorize this exact Run, admit its ledger, then execute one worker at a time.
---Failures and disposal settle callback once on the main loop; no retries occur.
---@param callback fun(ok: boolean, error_message?: string, summary: louiselm.workflow.BeadRunSummary)
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
