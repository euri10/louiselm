---@class louiselm.routing.Coordinator
---@field evidence louiselm.routing.Evidence
---@field approval louiselm.routing.Approval
---@field agents louiselm.routing.RoutingAgent[] Configured Agent inventory.
---@field recommend fun(self: louiselm.routing.Coordinator, phase: louiselm.routing.PhaseMetadata, state: louiselm.session.State): louiselm.routing.ApprovalPresentation?, louiselm.routing.Ranking?, string?
---@field observe fun(self: louiselm.routing.Coordinator, phase: louiselm.routing.PhaseMetadata, state: louiselm.session.State, outcome: string): boolean, string?
---@field feedback fun(self: louiselm.routing.Coordinator, phase: louiselm.routing.PhaseMetadata, state: louiselm.session.State, rating: string, context?: string): boolean, string?
---@field approve fun(self: louiselm.routing.Coordinator, candidate: unknown): louiselm.routing.ApprovalCandidate?, string?
---@field reject fun(self: louiselm.routing.Coordinator, candidate: unknown): boolean, string?
---@field clear_pending fun(self: louiselm.routing.Coordinator, phase: louiselm.routing.PhaseMetadata): boolean
---@field invalidate fun(self: louiselm.routing.Coordinator, phase: louiselm.routing.PhaseMetadata): boolean

local Approval = require("louiselm.routing.approval")
local Evidence = require("louiselm.routing.evidence")
local Routing = require("louiselm.routing.routing")

local M = {}
local Coordinator = {}
Coordinator.__index = Coordinator

---@param values string[]
---@return string[]
local function copy_strings(values)
  local copy = {}
  for index, value in ipairs(values) do
    copy[index] = value
  end
  return copy
end

---@param definitions louiselm.agent.Definitions
---@return louiselm.routing.RoutingAgent[] agents
local function routing_agents(definitions)
  local agents = {}
  local names = {}
  for name in pairs(definitions) do
    names[#names + 1] = name
  end
  table.sort(names)
  for _, name in ipairs(names) do
    local definition = definitions[name]
    agents[#agents + 1] = {
      name = name,
      capabilities = copy_strings(definition.capabilities or {}),
    }
  end
  return agents
end

---@param options louiselm.session.ConfigOption[]
---@return string|boolean|nil model
local function model_value(options)
  for _, option in ipairs(options) do
    if option.category == "model" then
      return option.current_value
    end
  end
  return nil
end

---@param options louiselm.session.ConfigOption[]
---@return louiselm.routing.RoutingModel[] models
local function model_options(options)
  local models = {}
  for _, option in ipairs(options) do
    if option.category == "model" and option.type == "select" then
      for _, value in ipairs(option.options or {}) do
        models[#models + 1] = {
          value = value.value,
          name = value.name,
          description = value.description,
        }
      end
    end
  end
  return models
end

---@param state louiselm.session.State
---@return louiselm.routing.RoutingAgent[] agents
local function live_agents(self, state)
  local agents = {}
  local models = model_options(state.config_options)
  for _, configured in ipairs(self.agents) do
    local agent = {
      name = configured.name,
      capabilities = copy_strings(configured.capabilities or {}),
    }
    if configured.name == state.agent then
      agent.models = models
      agent.context = state.context and {
        size = state.context.size,
        used = state.context.used,
      } or nil
    end
    agents[#agents + 1] = agent
  end
  return agents
end

---@param phase louiselm.routing.PhaseMetadata
---@param state louiselm.session.State
---@return louiselm.routing.EvidenceTarget
local function evidence_target(phase, state)
  local options = {}
  for _, option in ipairs(state.config_options) do
    options[option.id] = option.current_value
  end
  return {
    phase = phase.primary,
    agent = state.agent,
    model = type(model_value(state.config_options)) == "string" and model_value(state.config_options) or nil,
    options = options,
  }
end

---Create the first workflow coordinator from normalized configured Agent definitions.
---@param definitions unknown Normalized named Agent definitions.
---@param evidence_path string Persistent local evidence path.
---@return louiselm.routing.Coordinator? coordinator
---@return string? error_message
function M.new(definitions, evidence_path)
  if type(definitions) ~= "table" then
    return nil, "workflow requires configured Agent definitions"
  end
  local agents = routing_agents(definitions)
  if #agents == 0 then
    return nil, "workflow requires at least one configured Agent"
  end
  local evidence, evidence_error = Evidence.new(evidence_path)
  if evidence == nil then
    return nil, evidence_error
  end
  return setmetatable({ evidence = evidence, approval = Approval.new(), agents = agents }, Coordinator), nil
end

---Rank and queue recommendations for a completed, phase-tagged Session turn.
---@param self louiselm.routing.Coordinator
---@param phase louiselm.routing.PhaseMetadata Canonical phase for the completed work.
---@param state louiselm.session.State Current Session state.
---@return louiselm.routing.ApprovalPresentation? pending
---@return louiselm.routing.Ranking? ranking
---@return string? error_message
function Coordinator:recommend(phase, state)
  if state.status ~= "ready" then
    return nil, nil, "recommendations require an idle session"
  end
  local existing = self.approval:pending(phase)
  if existing ~= nil then
    return existing, nil, nil
  end
  if self.approval:approved(phase) ~= nil then
    return nil, nil, nil
  end
  local evidence, evidence_error = self.evidence:evidence()
  if evidence == nil then
    return nil, nil, evidence_error
  end
  local ranking, ranking_error = Routing.rank({
    phase = phase,
    current = { agent = state.agent, model = model_value(state.config_options) },
    agents = live_agents(self, state),
    evidence = evidence,
  })
  if ranking == nil then
    return nil, nil, ranking_error
  end
  local queued, queue_error = self.approval:queue(phase, ranking, state.status)
  if not queued then
    return nil, ranking, queue_error
  end
  return self.approval:pending(phase), ranking, nil
end

---Record operational outcome for a completed phase turn.
---@param self louiselm.routing.Coordinator
---@param phase louiselm.routing.PhaseMetadata
---@param state louiselm.session.State
---@param outcome string `completed`, `failed`, or `cancelled`.
---@return boolean recorded
---@return string? error_message
function Coordinator:observe(phase, state, outcome)
  return self.evidence:observe(evidence_target(phase, state), outcome)
end

---Record phase-end human feedback for the active Agent, Model, and options.
---@param self louiselm.routing.Coordinator
---@param phase louiselm.routing.PhaseMetadata
---@param state louiselm.session.State
---@param rating string `good`, `skip`, or `poor`.
---@param context? string Required for poor feedback.
---@return boolean recorded
---@return string? error_message
function Coordinator:feedback(phase, state, rating, context)
  return self.evidence:feedback(evidence_target(phase, state), rating, context)
end

---Approve a queued recommendation for later UI/process execution.
---@param self louiselm.routing.Coordinator
---@param candidate unknown
---@return louiselm.routing.ApprovalCandidate? approved
---@return string? error_message
function Coordinator:approve(candidate)
  return self.approval:approve(candidate)
end

---Reject a queued recommendation and suppress it for its current phase.
---@param self louiselm.routing.Coordinator
---@param candidate unknown
---@return boolean rejected
---@return string? error_message
function Coordinator:reject(candidate)
  return self.approval:reject(candidate)
end

---Dismiss a presentation without recording approval or rejection.
---@param self louiselm.routing.Coordinator
---@param phase louiselm.routing.PhaseMetadata
---@return boolean cleared
function Coordinator:clear_pending(phase)
  return self.approval:clear_pending(phase)
end

---Invalidate phase-scoped routing decisions after the workflow enters a new phase.
---@param self louiselm.routing.Coordinator
---@param phase louiselm.routing.PhaseMetadata
---@return boolean invalidated
function Coordinator:invalidate(phase)
  return self.approval:invalidate(phase)
end

return M
