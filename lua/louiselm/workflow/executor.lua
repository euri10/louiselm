---Validated stage transitions and generated-work observations for one workflow Run.

local Escape = require("louiselm.workflow.escape")
local Schema = require("louiselm.workflow.schema")
local Validate = require("louiselm.workflow.validate")

local M = {}
local Executor = {}
Executor.__index = Executor

---@alias louiselm.workflow.LedgerResult "consumed"|"reserved"|"pending"|"exhausted"

---@class louiselm.workflow.ExecutorLedger
---@field consume? fun(mutation_id: string, kind: string, units: integer): louiselm.workflow.LedgerResult, string?
---@field reserve? fun(mutation_id: string, kind: string, units: integer): louiselm.workflow.LedgerResult, string?
---@field confirm? fun(mutation_id: string, output_id: string): boolean, string?
---@field release? fun(mutation_id: string): boolean, string?

---@class louiselm.workflow.ExecutorOptions
---@field ledger? louiselm.workflow.ExecutorLedger Run-owned generated-work ledger.

---@class louiselm.workflow.TransitionResult
---@field from string Stage that produced the outcome.
---@field outcome string Outcome that was applied.
---@field to? string Next stage, omitted for terminal outcomes.
---@field terminal boolean Whether the workflow completed.
---@field exhausted? boolean Whether a bounded outcome selected its exhaustion outcome.
---@field requested_outcome? string Outcome that caused exhaustion routing.

---@class louiselm.workflow.WorkflowExecutor
---@field workflow string
---@field manifest table<string, table>
---@field current_stage_name string?
---@field status "active"|"completed"|"parked"|"cancelled"|"disposed"
---@field iterations table<string, integer>
---@field history louiselm.workflow.TransitionResult[]
---@field generator { mutation_id: string, maximum: integer, outputs: integer }?
---@field ledger louiselm.workflow.ExecutorLedger
---@field current_stage fun(self: louiselm.workflow.WorkflowExecutor): string?
---@field inspect fun(self: louiselm.workflow.WorkflowExecutor): table
---@field advance fun(self: louiselm.workflow.WorkflowExecutor, outcome_name: string, mutation_id?: string): louiselm.workflow.TransitionResult?, string?
---@field begin_generator fun(self: louiselm.workflow.WorkflowExecutor, mutation_id: string): boolean, string?
---@field record_generator_output fun(self: louiselm.workflow.WorkflowExecutor, output_id: string): boolean, string?
---@field end_generator fun(self: louiselm.workflow.WorkflowExecutor): boolean, string?
---@field park fun(self: louiselm.workflow.WorkflowExecutor): boolean, string?
---@field cancel fun(self: louiselm.workflow.WorkflowExecutor): boolean, string?
---@field resume fun(self: louiselm.workflow.WorkflowExecutor): boolean, string?
---@field dispose fun(self: louiselm.workflow.WorkflowExecutor): boolean, string?

---@param value unknown
---@return boolean
local function non_empty_string(value)
  return type(value) == "string" and value ~= ""
end

---@param manifest table<string, table>
---@return string? entry
local function find_entry(manifest)
  for name, block in pairs(manifest) do
    if type(block) == "table" and block.entry == true then
      return name
    end
  end
  return nil
end

---@param stage table
---@param outcome_name string
---@return table? outcome
local function find_outcome(stage, outcome_name)
  if type(stage.outcomes) ~= "table" then
    return nil
  end
  for _, outcome in ipairs(stage.outcomes) do
    if type(outcome) == "table" and outcome.name == outcome_name then
      return outcome
    end
  end
  return nil
end

---@param transition louiselm.workflow.TransitionResult
---@return louiselm.workflow.TransitionResult copy
local function copy_transition(transition)
  local copy = {
    from = transition.from,
    outcome = transition.outcome,
    to = transition.to,
    terminal = transition.terminal,
  }
  if transition.exhausted then
    copy.exhausted = true
  end
  if transition.requested_outcome ~= nil then
    copy.requested_outcome = transition.requested_outcome
  end
  return copy
end

---@param executor louiselm.workflow.WorkflowExecutor
---@return boolean, string?
local function active(executor)
  if executor.status == "parked" then
    return false, "workflow Run is parked"
  end
  if executor.status == "disposed" then
    return false, "workflow Run is disposed"
  end
  if executor.status == "cancelled" then
    return false, "workflow Run is cancelled"
  end
  if executor.status == "completed" then
    return false, "workflow Run is completed"
  end
  return true
end

---@param executor louiselm.workflow.WorkflowExecutor
---@param outcome table
---@param from string
---@param outcome_name string
---@param exhausted boolean
---@param requested_outcome? string
---@return louiselm.workflow.TransitionResult
local function apply_transition(executor, outcome, from, outcome_name, exhausted, requested_outcome)
  ---@type louiselm.workflow.TransitionResult
  local result
  if outcome.terminal == true then
    executor.current_stage_name = nil
    executor.status = "completed"
    result = { from = from, outcome = outcome_name, terminal = true }
    if exhausted then
      result.exhausted = true
    end
  else
    executor.current_stage_name = outcome.to
    result = { from = from, outcome = outcome_name, to = outcome.to, terminal = false }
    if exhausted then
      result.exhausted = true
    end
  end
  if requested_outcome ~= nil then
    result.requested_outcome = requested_outcome
  end
  executor.history[#executor.history + 1] = copy_transition(result)
  return result
end

---@param executor louiselm.workflow.WorkflowExecutor
---@param stage_name string
---@param outcome table
---@param outcome_name string
---@param mutation_id? string
---@return louiselm.workflow.TransitionResult?, string?
local function apply_automatic_back_edge(executor, stage_name, outcome, outcome_name, mutation_id)
  local resolver = type(outcome.resolver) == "string" and outcome.resolver or Schema.DEFAULT_RESOLVER
  if outcome["back-edge"] ~= true or Schema.AUTOMATIC_RESOLVERS[resolver] ~= true then
    return apply_transition(executor, outcome, stage_name, outcome_name, false)
  end

  if type(mutation_id) ~= "string" or mutation_id == "" then
    return nil, "automatic back-edge requires a mutation id"
  end
  local maximum = outcome["max-iterations"]
  local edge_key = stage_name .. "\0" .. outcome_name
  local used = executor.iterations[edge_key] or 0
  if used >= maximum then
    local stage = executor.manifest[stage_name]
    local exhaustion_name = outcome["on-exhausted"]
    local exhaustion = find_outcome(stage, exhaustion_name)
    if exhaustion == nil then
      return nil, "automatic back-edge exhaustion outcome is not declared"
    end
    return apply_transition(executor, exhaustion, stage_name, exhaustion_name, true, outcome_name)
  end

  if type(executor.ledger.consume) ~= "function" then
    return nil, "workflow Run has no generated-work ledger"
  end
  local charge, charge_error = executor.ledger.consume(mutation_id, "back_edge", 1)
  if charge == "exhausted" then
    executor.status = "parked"
    return nil, charge_error or "workflow Run budget exhausted"
  end
  if charge ~= "consumed" then
    return nil, charge_error or "back-edge budget charge is pending"
  end
  executor.iterations[edge_key] = used + 1
  return apply_transition(executor, outcome, stage_name, outcome_name, false)
end

---Create an executor from a workflow's discovered stage manifest.
---@param workflow string Workflow name.
---@param manifest table<string, table> Discovered stage manifest.
---@param options? louiselm.workflow.ExecutorOptions
---@return louiselm.workflow.WorkflowExecutor? executor
---@return string|louiselm.workflow.Rejection[]? error Rejections or an actionable construction error.
function M.new(workflow, manifest, options)
  if not non_empty_string(workflow) then
    return nil, "workflow name must be a non-empty string"
  end
  if type(manifest) ~= "table" then
    return nil, "workflow manifest must be a table"
  end
  if options ~= nil and type(options) ~= "table" then
    return nil, "workflow executor options must be a table"
  end
  options = options or {}
  local synthesized = Escape.apply(workflow, manifest)
  local validation = Validate.validate(workflow, synthesized)
  if not validation.ok then
    return nil, validation.rejections
  end
  local entry = find_entry(synthesized)
  if entry == nil then
    return nil, "validated workflow has no entry stage"
  end
  local ledger = options.ledger or {}
  if type(ledger) ~= "table" then
    return nil, "workflow executor ledger must be a table"
  end
  return setmetatable({
    workflow = workflow,
    manifest = synthesized,
    current_stage_name = entry,
    status = "active",
    iterations = {},
    history = {},
    generator = nil,
    ledger = ledger,
  }, Executor)
end

---Return the stage currently awaiting an outcome.
---@param self louiselm.workflow.WorkflowExecutor
---@return string? stage_name
function Executor:current_stage()
  return self.current_stage_name
end

---Return a safe snapshot of transition and generator state.
---@param self louiselm.workflow.WorkflowExecutor
---@return table snapshot
function Executor:inspect()
  local iterations = {}
  for edge, count in pairs(self.iterations) do
    iterations[edge] = count
  end
  local snapshot = {
    status = self.status,
    current_stage = self.current_stage_name,
    iterations = iterations,
    history = {},
  }
  for index, transition in ipairs(self.history) do
    snapshot.history[index] = copy_transition(transition)
  end
  if self.generator ~= nil then
    snapshot.generator = {
      mutation_id = self.generator.mutation_id,
      maximum = self.generator.maximum,
      outputs = self.generator.outputs,
    }
  end
  return snapshot
end

---Apply one model-selected named outcome and follow its validated transition.
---@param self louiselm.workflow.WorkflowExecutor
---@param outcome_name string Named outcome selected by the current stage.
---@param mutation_id? string Idempotency identity for an automatic back-edge charge.
---@return louiselm.workflow.TransitionResult? result
---@return string? error_message
function Executor:advance(outcome_name, mutation_id)
  local is_active, active_error = active(self)
  if not is_active then
    return nil, active_error
  end
  if self.generator ~= nil then
    return nil, "finish the active Generator before advancing the workflow"
  end
  if not non_empty_string(outcome_name) then
    return nil, "outcome name must be a non-empty string"
  end
  local stage_name = self.current_stage_name
  if stage_name == nil then
    return nil, "workflow Run has no current stage"
  end
  local stage = self.manifest[stage_name]
  local outcome = find_outcome(stage, outcome_name)
  if outcome == nil then
    return nil, string.format("outcome '%s' is not declared by stage '%s'", outcome_name, stage_name)
  end
  return apply_automatic_back_edge(self, stage_name, outcome, outcome_name, mutation_id)
end

---Reserve the declared maximum for a work-producing stage Generator.
---@param self louiselm.workflow.WorkflowExecutor
---@param mutation_id string Idempotency identity for the reservation.
---@return boolean started
---@return string? error_message
function Executor:begin_generator(mutation_id)
  local is_active, active_error = active(self)
  if not is_active then
    return false, active_error
  end
  if self.generator ~= nil then
    return false, "a Generator is already active"
  end
  if not non_empty_string(mutation_id) then
    return false, "Generator requires a mutation id"
  end
  local stage = self.current_stage_name and self.manifest[self.current_stage_name] or nil
  local maximum = type(stage) == "table" and type(stage.generates) == "table" and stage.generates.max or nil
  if type(maximum) ~= "number" or maximum < 1 or maximum % 1 ~= 0 then
    return false, "current stage is not a work-producing Generator"
  end
  if type(self.ledger.reserve) ~= "function" then
    return false, "workflow Run has no generated-work ledger"
  end
  local reservation, reserve_error = self.ledger.reserve(mutation_id, "skill_generator", maximum)
  if reservation == "exhausted" then
    self.status = "parked"
    return false, reserve_error or "workflow Run budget exhausted"
  end
  if reservation ~= "reserved" then
    return false, reserve_error or "Generator budget reservation is pending"
  end
  self.generator = { mutation_id = mutation_id, maximum = maximum, outputs = 0 }
  return true
end

---Confirm one output from the active Generator.
---@param self louiselm.workflow.WorkflowExecutor
---@param output_id string External identity of the generated output.
---@return boolean confirmed
---@return string? error_message
function Executor:record_generator_output(output_id)
  local is_active, active_error = active(self)
  if not is_active then
    return false, active_error
  end
  if self.generator == nil then
    return false, "no Generator is active"
  end
  if not non_empty_string(output_id) then
    return false, "Generator output id must be a non-empty string"
  end
  if self.generator.outputs >= self.generator.maximum then
    return false, "Generator output exceeds its declared maximum"
  end
  if type(self.ledger.confirm) ~= "function" then
    return false, "workflow Run has no generated-work ledger"
  end
  local confirmed, confirm_error = self.ledger.confirm(self.generator.mutation_id, output_id)
  if not confirmed then
    return false, confirm_error or "Generator output could not be confirmed"
  end
  self.generator.outputs = self.generator.outputs + 1
  return true
end

---Release unused capacity and finish the active Generator.
---@param self louiselm.workflow.WorkflowExecutor
---@return boolean completed
---@return string? error_message
function Executor:end_generator()
  local is_active, active_error = active(self)
  if not is_active then
    return false, active_error
  end
  if self.generator == nil then
    return false, "no Generator is active"
  end
  if type(self.ledger.release) ~= "function" then
    return false, "workflow Run has no generated-work ledger"
  end
  local released, release_error = self.ledger.release(self.generator.mutation_id)
  if not released then
    return false, release_error or "Generator reservation could not be released"
  end
  self.generator = nil
  return true
end

---Park the workflow without advancing its current stage.
---@param self louiselm.workflow.WorkflowExecutor
---@return boolean parked
---@return string? error_message
function Executor:park()
  if self.status == "disposed" then
    return false, "workflow Run is disposed"
  end
  if self.status == "completed" then
    return false, "workflow Run is completed"
  end
  self.status = "parked"
  return true
end

---Cancel the workflow without advancing its current stage.
---@param self louiselm.workflow.WorkflowExecutor
---@return boolean cancelled
---@return string? error_message
function Executor:cancel()
  if self.status == "disposed" then
    return false, "workflow Run is disposed"
  end
  if self.status == "completed" then
    return false, "workflow Run is completed"
  end
  self.status = "cancelled"
  return true
end

---Resume a Parked workflow with its current stage and counters intact.
---@param self louiselm.workflow.WorkflowExecutor
---@return boolean resumed
---@return string? error_message
function Executor:resume()
  if self.status == "disposed" then
    return false, "workflow Run is disposed"
  end
  if self.status == "completed" then
    return false, "workflow Run is completed"
  end
  if self.status == "cancelled" then
    return false, "workflow Run is cancelled"
  end
  if self.status ~= "parked" then
    return false, "workflow Run is not parked"
  end
  self.status = "active"
  return true
end

---Dispose the workflow and release a pending Generator reservation.
---@param self louiselm.workflow.WorkflowExecutor
---@return boolean disposed
---@return string? error_message
function Executor:dispose()
  if self.status == "disposed" then
    return true
  end
  if self.generator ~= nil then
    if type(self.ledger.release) ~= "function" then
      return false, "workflow Run has no generated-work ledger"
    end
    local released, release_error = self.ledger.release(self.generator.mutation_id)
    if not released then
      return false, release_error or "Generator reservation could not be released"
    end
    self.generator = nil
  end
  self.status = "disposed"
  return true
end

return M
