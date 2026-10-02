---@class louiselm.routing.RoutingModel
---@field value string Advertised model option value.
---@field name string Advertised display name.
---@field description? string Advertised description.

---@class louiselm.routing.RoutingAgent
---@field name string Configured agent name.
---@field capabilities? string[] Traits the agent is configured to declare.
---@field models? louiselm.routing.RoutingModel[] Models advertised in the agent's model config option.
---@field context? { size: number, used: number } Context usage, known only for an agent with a live session.
---@field available? boolean Whether the agent can be started; absent means not checked.

---@class louiselm.routing.RoutingEvidence
---@field phase? string Canonical phase the record was gathered in; absent makes the record global.
---@field agent string Agent the record describes.
---@field model? string Model the record describes.
---@field samples integer Number of observations behind the record.
---@field reliability number Observed operational success, from 0 to 1.
---@field quality? number Reported qualitative feedback, from 0 to 1.

---@class louiselm.routing.RoutingConstraints
---@field require_traits? string[] Traits a candidate must declare.
---@field allow_handoff? boolean Whether a new session with another agent may be recommended; defaults to true.

---@class louiselm.routing.RoutingRequest
---@field phase louiselm.routing.PhaseMetadata Phase the recommendation is scoped to.
---@field current { agent: string, model?: string } Choice in effect, always ranked as CONTINUE.
---@field agents louiselm.routing.RoutingAgent[] Configured agents, in any order.
---@field evidence? louiselm.routing.RoutingEvidence[] Local evidence, in any order.
---@field constraints? louiselm.routing.RoutingConstraints Explicit hard constraints.

---@class louiselm.routing.RoutingCandidate
---@field action "continue"|"model"|"handoff" Keeping the choice, changing model in session, or starting a new one.
---@field agent string Agent the candidate would run on.
---@field model? string Model the candidate would run, absent when the agent advertises none.
---@field score number Ranking score, higher is better.
---@field confidence number Confidence in the score, from 0 to 1.
---@field reasons string[] Human-readable explanation of the score.

---@class louiselm.routing.RoutingRejection
---@field action "continue"|"model"|"handoff"
---@field agent string
---@field model? string
---@field reasons string[] Every hard constraint the candidate failed, so a near miss is legible.

---@class louiselm.routing.Ranking
---@field candidates louiselm.routing.RoutingCandidate[] Ranked survivors, empty when every candidate failed a constraint.
---@field rejected louiselm.routing.RoutingRejection[] Near misses with the constraints they failed.

local Phase = require("louiselm.routing.phase")
local Provider = require("louiselm.agent.provider")
local Schema = require("louiselm.schema")
local Qualification = require("louiselm.routing.qualification")
local M = {}

---@class louiselm.routing.SelectionScope
---@field workload louiselm.routing.ComparisonWorkload
---@field baseline louiselm.routing.ComparisonRoute
---@field candidate louiselm.routing.ComparisonRoute
---@field policy_revision string
---@field effort_option_id? string

---Shared closed rule schema for setup and headless Agent configuration.
---@type louiselm.schema.Schema
M.rules_schema = assert(Schema.define({
  rules = {
    type = "map-of",
    optional = true,
    description = "Exact workload keys mapped to candidate pairs. Selection requires a matching approved comparison and comparable economics; explicit workload, selected Skill, then explicit phase resolve the key.",
    validator = function(value)
      return value[""] == nil, "rule workload keys must be non-empty"
    end,
    items = {
      type = "table",
      fields = {
        model = {
          type = "string",
          validator = function(value)
            return value ~= "", "must be non-empty"
          end,
          description = "Candidate advertised Model value.",
        },
        effort = {
          type = "string",
          optional = true,
          validator = function(value)
            return value ~= "", "must be non-empty"
          end,
          description = "Candidate advertised thought level; omission requires no effort option.",
        },
        policy_revision = {
          type = "string",
          validator = function(value)
            return value ~= "", "must be non-empty"
          end,
          description = "Exact approved comparison policy revision.",
        },
        require_traits = {
          type = "array-of",
          optional = true,
          items = {
            type = "string",
            validator = function(value)
              return value ~= "", "must be non-empty"
            end,
          },
          description = "Configured Agent capabilities required for this rule; they never establish Model quality.",
        },
      },
    },
  },
}))

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
---@param metadata louiselm.routing.PhaseMetadata
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
---@param evidence? louiselm.routing.RoutingEvidence[]
---@param phase string
---@param agent string
---@param model? string
---@return louiselm.routing.RoutingEvidence? record
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

---@param record? louiselm.routing.RoutingEvidence
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

---@param a louiselm.routing.RoutingCandidate
---@param b louiselm.routing.RoutingCandidate
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

---@param a louiselm.routing.RoutingRejection
---@param b louiselm.routing.RoutingRejection
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
---@param request unknown louiselm.routing.RoutingRequest
---@return louiselm.routing.Ranking? ranking
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

local function category(options, name)
  for _, option in ipairs(options) do
    if option.category == name then
      return option
    end
  end
end

local function only(value, keys)
  if type(value) ~= "table" then
    return false
  end
  for key in pairs(value) do
    if not keys[key] then
      return false
    end
  end
  return true
end

local function nonempty(value)
  return type(value) == "string" and value ~= ""
end

---Validate explicit, payload-free submission metadata without inferring from prose.
---@param value unknown Optional submission metadata.
---@return boolean valid False for unknown fields or contradictory phase metadata.
function M.valid_metadata(value)
  if value == nil then
    return true
  end
  if not only(value, { parent_turn_id = true, workload = true, skill = true, phase = true }) then
    return false
  end
  for _, key in ipairs({ "parent_turn_id", "workload", "skill" }) do
    if value[key] ~= nil and not nonempty(value[key]) then
      return false
    end
  end
  local phase = value.phase
  if phase == nil then
    return true
  end
  if not only(phase, { primary = true, secondary = true, source = true, confidence = true }) then
    return false
  end
  local parsed, parse_error = Phase.parse({ primary = phase.primary, secondary = phase.secondary })
  return parsed ~= nil
    and parse_error == nil
    and (phase.source == "explicit" or phase.source == "inferred")
    and type(phase.confidence) == "number"
    and phase.confidence >= 0
    and phase.confidence <= 1
    and (phase.source ~= "explicit" or phase.confidence == 1)
end

local function supports(option, value)
  if option == nil or option.type ~= "select" then
    return false
  end
  for _, choice in ipairs(option.options or {}) do
    if choice.value == value then
      return true
    end
  end
  return false
end

---Resolve a rule from explicit metadata and the current advertised authority.
---Pure: neither prompt prose, legacy scores nor Account limits are consulted.
---@param request { agent: string, definition: louiselm.agent.Definition, options: louiselm.session.ConfigOption[], metadata?: louiselm.routing.SubmissionMetadata }
---@return louiselm.routing.SelectionScope? scope Nil selects the baseline.
---@return string? reason Typed fallback reason.
function M.scope(request)
  local definition, metadata = request.definition, request.metadata or {}
  local auto = definition.auto
  local rules = auto and auto.rules or {}
  local key = metadata.workload
  if key == nil and metadata.skill ~= nil and rules[metadata.skill] ~= nil then
    key = metadata.skill
  end
  if
    key == nil
    and metadata.phase ~= nil
    and metadata.phase.source == "explicit"
    and Phase.is_canonical(metadata.phase.primary)
  then
    key = metadata.phase.primary
  end
  if key == nil then
    return nil, "workload_unknown"
  end
  local rule = rules[key]
  if auto == nil or rule == nil then
    return nil, "rule_missing"
  end
  local declared = set_of(definition.capabilities)
  for _, trait in ipairs(rule.require_traits or {}) do
    if not declared[trait] then
      return nil, "capability_unavailable"
    end
  end
  local model, effort = category(request.options, "model"), category(request.options, "thought_level")
  if
    not supports(model, rule.model)
    or not supports(model, auto.model)
    or (effort == nil and rule.effort ~= nil)
    or (effort ~= nil and not supports(effort, rule.effort))
    or (effort == nil and auto.effort ~= nil)
    or (effort ~= nil and not supports(effort, auto.effort))
  then
    return nil, "option_unsupported"
  end
  if model == nil then
    return nil, "option_unsupported"
  end
  local function route(pair)
    local values = {}
    for _, option in ipairs(request.options) do
      values[option.id] = option.current_value
    end
    values[model.id] = pair.model
    if effort ~= nil then
      values[effort.id] = pair.effort
    end
    local provider = Provider.resolve(definition.provider, values)
    if provider == nil then
      return nil
    end
    return {
      agent = request.agent,
      provider = provider,
      model = pair.model,
      model_option_id = model.id,
      options = values,
    }
  end
  local baseline, candidate = route(auto), route(rule)
  if baseline == nil or candidate == nil then
    return nil, "provider_unresolved"
  end
  return {
    workload = { kind = "main", id = key },
    baseline = baseline,
    candidate = candidate,
    policy_revision = rule.policy_revision,
    effort_option_id = effort and effort.id,
  },
    nil
end

local function same_route(left, right)
  if type(left) ~= "table" or type(left.options) ~= "table" then
    return false
  end
  for _, key in ipairs({ "agent", "provider", "model", "model_option_id" }) do
    if left[key] ~= right[key] then
      return false
    end
  end
  for key, value in pairs(left.options) do
    if right.options[key] ~= value then
      return false
    end
  end
  for key, value in pairs(right.options) do
    if left.options[key] ~= value then
      return false
    end
  end
  return true
end

local function economic_reason(approved)
  local groups, costs = {}, false
  for _, entry in ipairs(approved.economics or {}) do
    if entry.metric == "api_cost" or entry.metric == "quota" then
      if entry.route ~= "baseline" and entry.route ~= "candidate" then
        return "economics_unknown"
      end
      local key = entry.metric .. ":" .. entry.unit
      local group = groups[key] or { metric = entry.metric, unit = entry.unit }
      if group[entry.route] ~= nil then
        return "economics_unknown"
      end
      group[entry.route], groups[key] = entry, group
      if entry.metric == "api_cost" then
        costs = true
      end
    end
  end
  if not costs then
    return "economics_unknown"
  end
  local extra, cheaper, quota_better = {}, false, false
  for _, group in pairs(groups) do
    local baseline, candidate = group.baseline, group.candidate
    if baseline == nil or candidate == nil or baseline.kind ~= candidate.kind then
      return "economics_unknown"
    end
    if group.metric == "api_cost" then
      if candidate.value > baseline.value then
        extra[group.unit] = candidate.value - baseline.value
      end
      if candidate.value < baseline.value then
        cheaper = true
      end
    else
      if candidate.value > baseline.value then
        return "economics_worse"
      end
      if candidate.value < baseline.value then
        quota_better = true
      end
    end
  end
  if next(extra) ~= nil then
    if not quota_better then
      return "no_economic_benefit"
    end
    local allowance = approved.api_for_quota
    if allowance == nil then
      return "allowance_missing"
    end
    for currency, value in pairs(extra) do
      if allowance.max_extra_cost.currency ~= currency or value > allowance.max_extra_cost.value then
        return "allowance_insufficient"
      end
    end
  elseif not cheaper and not quota_better then
    return "no_economic_benefit"
  end
end

---Select a same-Agent pair only from an exact approved comparison and economics.
---Figures are operator-approved workload quantities, separated by metric, currency
---and observation kind. The allowance bounds this workload's extra planned cost;
---neither estimates nor selection establish actual billing or remaining quota.
---@param scope louiselm.routing.SelectionScope Validated live scope.
---@param approved? louiselm.routing.QualificationResult Validated durable lookup result.
---@return louiselm.routing.SelectionDecision decision Baseline for absent or ineligible evidence.
function M.select(scope, approved)
  local function pair(route)
    return { model = route.model, effort = scope.effort_option_id and route.options[scope.effort_option_id] or nil }
  end
  local decision = {
    reason = "baseline",
    requested = pair(scope.baseline),
    baseline = pair(scope.baseline),
    rule = scope.workload.id,
    workload = scope.workload,
  }
  if approved == nil then
    decision.fallback_reason = "unqualified"
    return decision
  end
  local report = approved.report
  if
    report == nil
    or report.workload.kind ~= scope.workload.kind
    or report.workload.id ~= scope.workload.id
    or report.policy_revision ~= scope.policy_revision
    or not same_route(report.baseline, scope.baseline)
    or not same_route(report.candidate, scope.candidate)
  then
    decision.fallback_reason = "unqualified"
    return decision
  end
  decision.qualification =
    { report_id = approved.report_id, policy_revision = approved.policy_revision, revision = approved.revision }
  decision.economic_basis, decision.api_for_quota = approved.economics, approved.api_for_quota
  decision.fallback_reason = economic_reason(approved)
  if decision.fallback_reason == nil then
    decision.reason, decision.requested = "qualified", pair(scope.candidate)
  end
  return decision
end

---Check the closed, payload-free decision before durable recording.
---@param value unknown Selection provenance supplied by admission.
---@return boolean valid Structural validity, never a substitute for live approval lookup.
function M.valid_selection(value)
  if
    not only(value, {
      reason = true,
      requested = true,
      baseline = true,
      rule = true,
      workload = true,
      fallback_reason = true,
      qualification = true,
      economic_basis = true,
      api_for_quota = true,
    })
  then
    return false
  end
  for _, pair in ipairs({ value.requested, value.baseline }) do
    if
      not only(pair, { model = true, effort = true })
      or not nonempty(pair.model)
      or (pair.effort ~= nil and not nonempty(pair.effort))
    then
      return false
    end
  end
  if type(value.requested) ~= "table" or type(value.baseline) ~= "table" then
    return false
  end
  if value.rule ~= nil and not nonempty(value.rule) then
    return false
  end
  if
    value.workload ~= nil
    and (
      not only(value.workload, { kind = true, id = true })
      or value.workload.kind ~= "main"
      or value.workload.id ~= value.rule
    )
  then
    return false
  end
  if
    value.fallback_reason ~= nil
    and not ({
      workload_unknown = true,
      rule_missing = true,
      capability_unavailable = true,
      option_unsupported = true,
      provider_unresolved = true,
      unqualified = true,
      economics_unknown = true,
      economics_worse = true,
      no_economic_benefit = true,
      allowance_missing = true,
      allowance_insufficient = true,
      approval_unavailable = true,
      selection_incomplete = true,
    })[value.fallback_reason]
  then
    return false
  end
  local approved = value.qualification
  if approved ~= nil then
    if
      not only(approved, { report_id = true, policy_revision = true, revision = true, approval_revision = true })
      or not nonempty(approved.report_id)
      or not nonempty(approved.policy_revision)
      or type(approved.revision) ~= "number"
      or approved.revision < 1
      or approved.revision % 1 ~= 0
      or (
        approved.approval_revision ~= nil
        and (
          type(approved.approval_revision) ~= "number"
          or approved.approval_revision < approved.revision
          or approved.approval_revision % 1 ~= 0
        )
      )
    then
      return false
    end
  end
  if not Qualification.valid_economics(value.economic_basis, value.api_for_quota) then
    return false
  end
  if value.reason == "qualified" then
    return value.rule ~= nil
      and value.workload ~= nil
      and approved ~= nil
      and value.fallback_reason == nil
      and economic_reason({ economics = value.economic_basis, api_for_quota = value.api_for_quota }) == nil
  end
  return value.reason == "baseline"
    and value.requested.model == value.baseline.model
    and value.requested.effort == value.baseline.effort
end

return M
