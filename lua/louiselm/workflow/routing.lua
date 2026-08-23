---@class louiselm.workflow.RoutingModel
---@field value string Advertised model option value.
---@field name string Advertised display name.
---@field description? string Advertised description.

---@class louiselm.workflow.RoutingAgent
---@field name string Configured agent name.
---@field capabilities? string[] Traits the agent is configured to declare.
---@field models? louiselm.workflow.RoutingModel[] Models advertised in the agent's model config option.
---@field context? { size: number, used: number } Context usage, known only for an agent with a live session.
---@field available? boolean Whether the agent can be started; absent means not checked.

---@class louiselm.workflow.RoutingEvidence
---@field phase? string Canonical phase the record was gathered in; absent makes the record global.
---@field agent string Agent the record describes.
---@field model? string Model the record describes.
---@field samples integer Number of observations behind the record.
---@field reliability number Observed operational success, from 0 to 1.
---@field quality? number Reported qualitative feedback, from 0 to 1.

---@class louiselm.workflow.RoutingConstraints
---@field require_traits? string[] Traits a candidate must declare.
---@field allow_handoff? boolean Whether a new session with another agent may be recommended; defaults to true.

---@class louiselm.workflow.RoutingRequest
---@field phase louiselm.workflow.PhaseMetadata Phase the recommendation is scoped to.
---@field current { agent: string, model?: string } Choice in effect, always ranked as CONTINUE.
---@field agents louiselm.workflow.RoutingAgent[] Configured agents, in any order.
---@field evidence? louiselm.workflow.RoutingEvidence[] Local evidence, in any order.
---@field constraints? louiselm.workflow.RoutingConstraints Explicit hard constraints.

---@class louiselm.workflow.RoutingCandidate
---@field action "continue"|"model"|"handoff" Keeping the choice, changing model in session, or starting a new one.
---@field agent string Agent the candidate would run on.
---@field model? string Model the candidate would run, absent when the agent advertises none.
---@field score number Ranking score, higher is better.
---@field confidence number Confidence in the score, from 0 to 1.
---@field reasons string[] Human-readable explanation of the score.

---@class louiselm.workflow.RoutingRejection
---@field action "continue"|"model"|"handoff"
---@field agent string
---@field model? string
---@field reasons string[] Every hard constraint the candidate failed, so a near miss is legible.

---@class louiselm.workflow.Ranking
---@field candidates louiselm.workflow.RoutingCandidate[] Ranked survivors, empty when every candidate failed a constraint.
---@field rejected louiselm.workflow.RoutingRejection[] Near misses with the constraints they failed.

local Phase = require("louiselm.workflow.phase")
local M = {}

--- Built-in phase profiles. They express traits and headroom requirements rather
--- than named models, so an agent inventory that changes underneath does not
--- invalidate them. `prefers` traits are matched against the `capabilities` an
--- agent is configured to declare; `min_headroom` is the share of the context
--- window a phase needs free before it is worth starting there.
local PROFILES = {
  design = { prefers = { "reasoning" }, min_headroom = 0.5 },
  planning = { prefers = { "reasoning" }, min_headroom = 0.4 },
  implementation = { prefers = { "coding" }, min_headroom = 0.3 },
  review = { prefers = { "reasoning", "coding" }, min_headroom = 0.3 },
  qa = { prefers = { "coding" }, min_headroom = 0.2 },
  mechanical = { prefers = { "speed" }, min_headroom = 0.1 },
}

local TRAIT_WEIGHT = 0.4
local EVIDENCE_WEIGHT = 0.4
local HEADROOM_WEIGHT = 0.2

--- Changing model costs a turn of the agent's attention; a handoff costs a whole
--- session. Both must be earned back by a better score, which is what stops a
--- marginal improvement from churning the session.
local MODEL_PENALTY = 0.05
local HANDOFF_PENALTY = 0.15

--- What an unmeasured component contributes. Deliberately mid-range: an unknown
--- must neither reject a candidate nor recommend one, it must only make the
--- surrounding evidence decide.
local NEUTRAL = 0.5

--- Confidence contributed by a component nothing backs.
local UNKNOWN_CONFIDENCE = 0.5

--- Observations before evidence is trusted completely.
local CONFIDENT_SAMPLES = 5

--- Evidence gathered in another phase still says something about an agent, but
--- less than evidence gathered in this one.
local GLOBAL_EVIDENCE_CONFIDENCE = 0.75

--- A secondary phase shapes the work without defining it, so its traits count,
--- but not as much as the primary phase's.
local SECONDARY_TRAIT_WEIGHT = 0.5

---@param value number
---@return integer
local function percent(value)
  return math.floor(value * 100 + 0.5)
end

---@param values? string[]
---@return table<string, boolean>
local function set_of(values)
  local set = {}
  for _, value in ipairs(values or {}) do
    set[value] = true
  end
  return set
end

---Weight every trait the phase profiles prefer, primary phase first.
---@param metadata louiselm.workflow.PhaseMetadata
---@return string[] traits Deterministic order: primary profile order, then each secondary's.
---@return table<string, number> weights
---@return number min_headroom
local function phase_traits(metadata)
  local traits = {}
  local weights = {}
  local min_headroom = 0

  local phases = { { name = metadata.primary, weight = 1 } }
  for _, secondary in ipairs(metadata.secondary) do
    phases[#phases + 1] = { name = secondary, weight = SECONDARY_TRAIT_WEIGHT }
  end

  for _, phase in ipairs(phases) do
    local profile = PROFILES[phase.name]
    if profile ~= nil then
      min_headroom = math.max(min_headroom, profile.min_headroom)
      for _, trait in ipairs(profile.prefers) do
        if weights[trait] == nil then
          traits[#traits + 1] = trait
          weights[trait] = phase.weight
        else
          weights[trait] = math.max(weights[trait], phase.weight)
        end
      end
    end
  end
  return traits, weights, min_headroom
end

---Find the evidence that best describes one candidate, preferring this phase.
---@param evidence? louiselm.workflow.RoutingEvidence[]
---@param phase string
---@param agent string
---@param model? string
---@return louiselm.workflow.RoutingEvidence? record
---@return boolean phase_specific
local function evidence_for(evidence, phase, agent, model)
  local global
  for _, record in ipairs(evidence or {}) do
    if record.agent == agent and record.model == model then
      if record.phase == phase then
        return record, true
      end
      if record.phase == nil and global == nil then
        global = record
      end
    end
  end
  return global, false
end

---@param record? louiselm.workflow.RoutingEvidence
---@return number score
local function evidence_score(record)
  if record == nil then
    return NEUTRAL
  end
  if record.quality ~= nil then
    return (record.reliability + record.quality) / 2
  end
  return record.reliability
end

---Collect every hard constraint a candidate fails, so a near miss is legible.
---@return string[] reasons Empty when the candidate passes.
local function rejections(agent, action, constraints, metadata, min_headroom)
  local reasons = {}
  if agent.available == false then
    reasons[#reasons + 1] = "agent is unavailable"
  end
  if action == "handoff" and constraints.allow_handoff == false then
    reasons[#reasons + 1] = "handoff is not allowed"
  end

  local declared = set_of(agent.capabilities)
  for _, trait in ipairs(constraints.require_traits or {}) do
    if not declared[trait] then
      reasons[#reasons + 1] = "does not declare " .. trait
    end
  end

  if agent.context ~= nil then
    local headroom = (agent.context.size - agent.context.used) / agent.context.size
    if headroom < min_headroom then
      reasons[#reasons + 1] =
        string.format("%d%% context free, %s needs %d%%", percent(headroom), metadata.primary, percent(min_headroom))
    end
  end
  return reasons
end

---@param a louiselm.workflow.RoutingCandidate
---@param b louiselm.workflow.RoutingCandidate
---@return boolean
local function before(a, b)
  if a.score ~= b.score then
    return a.score > b.score
  end
  if a.agent ~= b.agent then
    return a.agent < b.agent
  end
  return (a.model or "") < (b.model or "")
end

---@param a louiselm.workflow.RoutingRejection
---@param b louiselm.workflow.RoutingRejection
---@return boolean
local function rejected_before(a, b)
  if a.agent ~= b.agent then
    return a.agent < b.agent
  end
  return (a.model or "") < (b.model or "")
end

---Enumerate every candidate the current choice could move to, current one first.
---@return table[] entries
local function candidate_entries(agents, current)
  local entries = {}
  for _, agent in ipairs(agents) do
    local models = agent.models or {}
    if agent.name == current.agent then
      entries[#entries + 1] = { agent = agent, model = current.model, action = "continue" }
      for _, model in ipairs(models) do
        if model.value ~= current.model then
          entries[#entries + 1] = { agent = agent, model = model.value, action = "model" }
        end
      end
    elseif #models == 0 then
      entries[#entries + 1] = { agent = agent, model = nil, action = "handoff" }
    else
      for _, model in ipairs(models) do
        entries[#entries + 1] = { agent = agent, model = model.value, action = "handoff" }
      end
    end
  end
  return entries
end

---Describe the built-in profile for one canonical phase.
---@param phase unknown Canonical phase name.
---@return { prefers: string[], min_headroom: number }? profile A copy; nil outside the phase contract.
function M.profile(phase)
  if not Phase.is_canonical(phase) then
    return nil
  end
  local profile = PROFILES[phase]
  local prefers = {}
  for index, trait in ipairs(profile.prefers) do
    prefers[index] = trait
  end
  return { prefers = prefers, min_headroom = profile.min_headroom }
end

---Rank the routing candidates for one phase. Pure: it starts no process, opens no
---UI, and does not mutate the request.
---@param request unknown louiselm.workflow.RoutingRequest
---@return louiselm.workflow.Ranking? ranking
---@return string? error_message
function M.rank(request)
  if type(request) ~= "table" then
    return nil, "request must be a table"
  end
  local metadata = request.phase
  if type(metadata) ~= "table" or not Phase.is_canonical(metadata.primary) then
    return nil, "request requires phase metadata"
  end
  local current = request.current
  if type(current) ~= "table" or type(current.agent) ~= "string" then
    return nil, "request requires a current agent"
  end
  if type(request.agents) ~= "table" then
    return nil, "request requires an agents array"
  end

  local known = false
  for _, agent in ipairs(request.agents) do
    if agent.name == current.agent then
      known = true
    end
  end
  if not known then
    return nil, "current agent '" .. current.agent .. "' is not among the supplied agents"
  end

  local constraints = request.constraints or {}
  local traits, weights, min_headroom = phase_traits(metadata)
  local total_weight = 0
  for _, trait in ipairs(traits) do
    total_weight = total_weight + weights[trait]
  end

  local candidates = {}
  local rejected = {}
  for _, entry in ipairs(candidate_entries(request.agents, current)) do
    local agent = entry.agent
    local reasons = rejections(agent, entry.action, constraints, metadata, min_headroom)
    if #reasons > 0 then
      rejected[#rejected + 1] = { action = entry.action, agent = agent.name, model = entry.model, reasons = reasons }
    else
      local declared = set_of(agent.capabilities)
      local explanation = {}
      if entry.action == "continue" then
        explanation[1] = "current choice"
      elseif entry.action == "model" then
        explanation[1] = "changes model within the current session"
      else
        explanation[1] = "starts a new session with " .. agent.name
      end

      local matched_weight = 0
      for _, trait in ipairs(traits) do
        if declared[trait] then
          matched_weight = matched_weight + weights[trait]
          explanation[#explanation + 1] = "declares " .. trait
        else
          explanation[#explanation + 1] = "does not declare " .. trait
        end
      end
      local trait_match = total_weight > 0 and matched_weight / total_weight or NEUTRAL

      local record, phase_specific = evidence_for(request.evidence, metadata.primary, agent.name, entry.model)
      if record == nil then
        explanation[#explanation + 1] = "no " .. metadata.primary .. " evidence yet"
      elseif phase_specific then
        explanation[#explanation + 1] = string.format("%d %s samples", record.samples, metadata.primary)
      else
        explanation[#explanation + 1] = string.format("%d samples from other phases", record.samples)
      end

      local headroom = NEUTRAL
      if agent.context ~= nil then
        headroom = (agent.context.size - agent.context.used) / agent.context.size
      else
        explanation[#explanation + 1] = "context unknown"
      end

      local penalty = 0
      if entry.action == "model" then
        penalty = MODEL_PENALTY
      elseif entry.action == "handoff" then
        penalty = HANDOFF_PENALTY
      end

      local evidence_confidence = UNKNOWN_CONFIDENCE
      if record ~= nil then
        evidence_confidence = math.min(1, record.samples / CONFIDENT_SAMPLES)
        if not phase_specific then
          evidence_confidence = evidence_confidence * GLOBAL_EVIDENCE_CONFIDENCE
        end
      end
      local trait_confidence = next(declared) ~= nil and 1 or UNKNOWN_CONFIDENCE
      local context_confidence = agent.context ~= nil and 1 or UNKNOWN_CONFIDENCE

      candidates[#candidates + 1] = {
        action = entry.action,
        agent = agent.name,
        model = entry.model,
        score = TRAIT_WEIGHT * trait_match
          + EVIDENCE_WEIGHT * evidence_score(record)
          + HEADROOM_WEIGHT * headroom
          - penalty,
        confidence = metadata.confidence * (trait_confidence + evidence_confidence + context_confidence) / 3,
        reasons = explanation,
      }
    end
  end

  table.sort(candidates, before)
  table.sort(rejected, rejected_before)
  return { candidates = candidates, rejected = rejected }
end

return M
