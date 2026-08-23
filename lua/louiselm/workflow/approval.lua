---@class louiselm.workflow.ApprovalCandidate: louiselm.workflow.RoutingCandidate
---@field label? string Presentation label, present only while shown to a user.

---@class louiselm.workflow.ApprovalPresentation
---@field phase louiselm.workflow.PhaseMetadata Phase whose recommendations are shown.
---@field candidates louiselm.workflow.ApprovalCandidate[] Short ranked list awaiting approval.

---@class louiselm.workflow.ApprovalState
---@field phase louiselm.workflow.PhaseMetadata
---@field ranking louiselm.workflow.Ranking
---@field pending? louiselm.workflow.ApprovalPresentation
---@field approved? louiselm.workflow.ApprovalCandidate
---@field rejected table<string, boolean>

---@class louiselm.workflow.Approval
---@field states table<string, louiselm.workflow.ApprovalState>
---@field state_order string[]
---@field queue fun(self: louiselm.workflow.Approval, phase: louiselm.workflow.PhaseMetadata, ranking: louiselm.workflow.Ranking, session_status: string): boolean, string?
---@field pending fun(self: louiselm.workflow.Approval, phase: louiselm.workflow.PhaseMetadata): louiselm.workflow.ApprovalPresentation?
---@field approved fun(self: louiselm.workflow.Approval, phase: louiselm.workflow.PhaseMetadata): louiselm.workflow.ApprovalCandidate?
---@field approve fun(self: louiselm.workflow.Approval, candidate: unknown): louiselm.workflow.ApprovalCandidate?, string?
---@field reject fun(self: louiselm.workflow.Approval, candidate: unknown): boolean, string?
---@field clear_pending fun(self: louiselm.workflow.Approval, phase: louiselm.workflow.PhaseMetadata): boolean
---@field invalidate fun(self: louiselm.workflow.Approval, phase: louiselm.workflow.PhaseMetadata): boolean
---@field reconsider fun(self: louiselm.workflow.Approval, phase: louiselm.workflow.PhaseMetadata, session_status: string): boolean, string?

local Phase = require("louiselm.workflow.phase")
local M = {}
local Approval = {}
Approval.__index = Approval

local MAX_CANDIDATES = 5

---@param values string[]
---@return string[]
local function copy_strings(values)
  local copy = {}
  for index, value in ipairs(values) do
    copy[index] = value
  end
  return copy
end

---@param candidate louiselm.workflow.RoutingCandidate
---@param with_label? boolean
---@return louiselm.workflow.ApprovalCandidate
local function copy_candidate(candidate, with_label)
  local copy = {
    action = candidate.action,
    agent = candidate.agent,
    model = candidate.model,
    score = candidate.score,
    confidence = candidate.confidence,
    reasons = copy_strings(candidate.reasons),
  }
  if with_label then
    copy.label = string.format(
      "%s %s/%s · %d%% confidence · %s",
      string.upper(candidate.action),
      candidate.agent,
      candidate.model or "-",
      math.floor(candidate.confidence * 100 + 0.5),
      table.concat(candidate.reasons, "; ")
    )
  end
  return copy
end

---@param ranking louiselm.workflow.Ranking
---@return louiselm.workflow.Ranking
local function copy_ranking(ranking)
  local copy = { candidates = {}, rejected = {} }
  for index, candidate in ipairs(ranking.candidates) do
    copy.candidates[index] = copy_candidate(candidate)
  end
  for index, rejection in ipairs(ranking.rejected) do
    copy.rejected[index] = {
      action = rejection.action,
      agent = rejection.agent,
      model = rejection.model,
      reasons = copy_strings(rejection.reasons),
    }
  end
  return copy
end

---@param phase louiselm.workflow.PhaseMetadata
---@return string
local function phase_key(phase)
  return phase.primary .. "|" .. table.concat(phase.secondary, ",")
end

---@param phase unknown
---@return louiselm.workflow.PhaseMetadata? normalized
---@return string? error_message
local function normalize_phase(phase)
  if type(phase) ~= "table" or not Phase.is_canonical(phase.primary) then
    return nil, "phase requires canonical metadata"
  end
  if type(phase.secondary) ~= "table" then
    return nil, "phase requires canonical metadata"
  end

  local secondary = {}
  local seen = {}
  for _, value in ipairs(phase.secondary) do
    if not Phase.is_canonical(value) or value == phase.primary or seen[value] then
      return nil, "phase requires canonical metadata"
    end
    seen[value] = true
    secondary[#secondary + 1] = value
  end
  return {
    primary = phase.primary,
    secondary = secondary,
    source = phase.source,
    confidence = phase.confidence,
  },
    nil
end

---@param candidate louiselm.workflow.RoutingCandidate
---@return string
local function candidate_key(candidate)
  return table.concat({ candidate.action, candidate.agent, candidate.model or "" }, "\0")
end

---@param state louiselm.workflow.ApprovalState
---@return louiselm.workflow.ApprovalPresentation?
local function copy_pending(state)
  if state.pending == nil then
    return nil
  end
  local copy = { phase = state.phase, candidates = {} }
  for index, candidate in ipairs(state.pending.candidates) do
    copy.candidates[index] = copy_candidate(candidate, true)
  end
  return copy
end

---@param state louiselm.workflow.ApprovalState
---@return louiselm.workflow.ApprovalPresentation?
local function presentation(state)
  local candidates = {}
  for _, candidate in ipairs(state.ranking.candidates) do
    if not state.rejected[candidate_key(candidate)] then
      candidates[#candidates + 1] = copy_candidate(candidate, true)
      if #candidates == MAX_CANDIDATES then
        break
      end
    end
  end
  if #candidates == 0 then
    return nil
  end
  return { phase = state.phase, candidates = candidates }
end

---@param self louiselm.workflow.Approval
---@param candidate unknown
---@return louiselm.workflow.ApprovalState?
---@return louiselm.workflow.RoutingCandidate?
local function find_pending(self, candidate)
  if type(candidate) ~= "table" then
    return nil, nil
  end
  local key = candidate_key(candidate)
  for _, phase in ipairs(self.state_order) do
    local state = self.states[phase]
    if state ~= nil and state.pending ~= nil then
      for _, pending_candidate in ipairs(state.pending.candidates) do
        if candidate_key(pending_candidate) == key then
          return state, pending_candidate
        end
      end
    end
  end
  return nil, nil
end

---Create an in-memory phase recommendation approval lifecycle.
---@return louiselm.workflow.Approval approval
function M.new()
  return setmetatable({ states = {}, state_order = {} }, Approval)
end

---Queue ranked recommendations for an idle session.
---@param self louiselm.workflow.Approval
---@param phase louiselm.workflow.PhaseMetadata
---@param ranking louiselm.workflow.Ranking
---@param session_status string Current session status; only `ready` is idle.
---@return boolean ok
---@return string? error_message
function Approval:queue(phase, ranking, session_status)
  if session_status ~= "ready" then
    return false, "recommendations require an idle session"
  end
  local normalized, phase_error = normalize_phase(phase)
  if normalized == nil then
    return false, phase_error
  end
  local key = phase_key(normalized)
  local state = self.states[key]
  if state == nil then
    state = { phase = normalized, ranking = copy_ranking(ranking), rejected = {} }
    self.states[key] = state
    self.state_order[#self.state_order + 1] = key
  elseif state.approved ~= nil then
    return false, "phase already has an approved choice"
  elseif state.pending ~= nil then
    return false, "phase already has pending recommendations"
  else
    state.ranking = copy_ranking(ranking)
  end

  state.pending = presentation(state)
  if state.pending == nil then
    return false, "no unsuppressed recommendations"
  end
  return true, nil
end

---Return the detached recommendations currently awaiting approval.
---@param self louiselm.workflow.Approval
---@param phase louiselm.workflow.PhaseMetadata
---@return louiselm.workflow.ApprovalPresentation? pending
function Approval:pending(phase)
  local normalized = normalize_phase(phase)
  if normalized == nil then
    return nil
  end
  local state = self.states[phase_key(normalized)]
  return state and copy_pending(state) or nil
end

---Return the detached approved choice for a phase, if one exists.
---@param self louiselm.workflow.Approval
---@param phase louiselm.workflow.PhaseMetadata
---@return louiselm.workflow.RoutingCandidate? approved
function Approval:approved(phase)
  local normalized = normalize_phase(phase)
  if normalized == nil then
    return nil
  end
  local state = self.states[phase_key(normalized)]
  if state == nil or state.approved == nil then
    return nil
  end
  return copy_candidate(state.approved, true)
end

---Approve a candidate from the currently pending phase recommendation.
---@param self louiselm.workflow.Approval
---@param candidate unknown
---@return louiselm.workflow.RoutingCandidate? approved
---@return string? error_message
function Approval:approve(candidate)
  local state, pending_candidate = find_pending(self, candidate)
  if state == nil or pending_candidate == nil then
    return nil, "recommendation is not pending"
  end
  state.approved = copy_candidate(pending_candidate, true)
  state.pending = nil
  return copy_candidate(state.approved, true), nil
end

---Reject a pending candidate and suppress it for the current phase.
---@param self louiselm.workflow.Approval
---@param candidate unknown
---@return boolean ok
---@return string? error_message
function Approval:reject(candidate)
  local state, pending_candidate = find_pending(self, candidate)
  if state == nil or pending_candidate == nil then
    return false, "recommendation is not pending"
  end
  state.rejected[candidate_key(pending_candidate)] = true
  state.pending = presentation(state)
  return true, nil
end

---Dismiss the current presentation without approving or rejecting a choice.
---@param self louiselm.workflow.Approval
---@param phase louiselm.workflow.PhaseMetadata
---@return boolean ok
function Approval:clear_pending(phase)
  local normalized = normalize_phase(phase)
  if normalized == nil then
    return false
  end
  local state = self.states[phase_key(normalized)]
  if state == nil then
    return false
  end
  state.pending = nil
  return true
end

---Clear phase-scoped decisions so fresh routing can be presented.
---@param self louiselm.workflow.Approval
---@param phase louiselm.workflow.PhaseMetadata
---@return boolean ok
function Approval:invalidate(phase)
  local normalized = normalize_phase(phase)
  if normalized == nil then
    return false
  end
  local key = phase_key(normalized)
  if self.states[key] == nil then
    return false
  end
  self.states[key] = nil
  for index, value in ipairs(self.state_order) do
    if value == key then
      table.remove(self.state_order, index)
      break
    end
  end
  return true
end

---Lift rejection suppression and present the last ranking for a phase again.
---@param self louiselm.workflow.Approval
---@param phase louiselm.workflow.PhaseMetadata
---@param session_status string Current session status; only `ready` is idle.
---@return boolean ok
---@return string? error_message
function Approval:reconsider(phase, session_status)
  if session_status ~= "ready" then
    return false, "recommendations require an idle session"
  end
  local normalized, phase_error = normalize_phase(phase)
  if normalized == nil then
    return false, phase_error
  end
  local state = self.states[phase_key(normalized)]
  if state == nil then
    return false, "no recommendations to reconsider"
  end
  state.rejected = {}
  state.pending = nil
  state.approved = nil
  state.pending = presentation(state)
  if state.pending == nil then
    return false, "no recommendations to reconsider"
  end
  return true, nil
end

return M
