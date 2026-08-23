---@alias louiselm.workflow.PhaseName "design"|"planning"|"implementation"|"review"|"qa"|"mechanical"

---@class louiselm.workflow.PhaseMetadata
---@field primary louiselm.workflow.PhaseName Canonical phase the unit of work mainly belongs to.
---@field secondary louiselm.workflow.PhaseName[] Further canonical phases in declared order, never repeating the primary.
---@field source "explicit"|"inferred" Whether the metadata was declared or derived from a name.
---@field confidence number Confidence in the metadata, from 0 to 1.

local M = {}

--- Canonical phases in workflow order. This list is the routing contract: a phase
--- outside it has no built-in profile, so accepting one would rank candidates
--- against nothing.
local CANONICAL = { "design", "planning", "implementation", "review", "qa", "mechanical" }

--- Declared metadata states the author's intent, so it is trusted completely.
local EXPLICIT_CONFIDENCE = 1

--- A name matching a phase is weak evidence: `security-review` names a subject as
--- readily as a phase. Inference is therefore capped well below a declaration and
--- scaled by the share of the name that phase vocabulary actually explains, so a
--- fully-read `qa-review` outranks a half-read `security-review`.
local INFERRED_CEILING = 0.5

local CANONICAL_SET = {}
for _, phase in ipairs(CANONICAL) do
  CANONICAL_SET[phase] = true
end

local EXPECTED = "expected one of " .. table.concat(CANONICAL, ", ")

---@param value unknown
---@return string? error_message
local function unknown_phase(value)
  if type(value) ~= "string" or not CANONICAL_SET[value] then
    return "unknown phase '" .. tostring(value) .. "'; " .. EXPECTED
  end
  return nil
end

---@param value table
---@return boolean
local function dense_array(value)
  local length = 0
  while value[length + 1] ~= nil do
    length = length + 1
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
      return false
    end
  end
  return true
end

---Validate a declared secondary list against an already-validated primary.
---@param value unknown
---@param primary string
---@return string[]? secondary
---@return string? error_message
local function secondary_phases(value, primary)
  if value == nil then
    return {}
  end
  if type(value) ~= "table" or not dense_array(value) then
    return nil, "phase secondary must be an array of phase names"
  end

  local secondary = {}
  local seen = {}
  for _, phase in ipairs(value) do
    local phase_error = unknown_phase(phase)
    if phase_error ~= nil then
      return nil, phase_error
    end
    if phase == primary then
      return nil, "phase secondary must not repeat the primary phase '" .. primary .. "'"
    end
    if seen[phase] then
      return nil, "phase secondary must not repeat '" .. phase .. "'"
    end
    seen[phase] = true
    secondary[#secondary + 1] = phase
  end
  return secondary
end

---Canonical phases in workflow order.
---@return louiselm.workflow.PhaseName[] phases A copy; the contract is not caller-mutable.
function M.canonical()
  local phases = {}
  for index, phase in ipairs(CANONICAL) do
    phases[index] = phase
  end
  return phases
end

---@param value unknown
---@return boolean
function M.is_canonical(value)
  return type(value) == "string" and CANONICAL_SET[value] == true
end

---Validate declared phase metadata, either a bare phase name or a mapping.
---@param value unknown Declared `phase` value.
---@return louiselm.workflow.PhaseMetadata? metadata
---@return string? error_message
function M.parse(value)
  if type(value) == "string" then
    local phase_error = unknown_phase(value)
    if phase_error ~= nil then
      return nil, phase_error
    end
    return { primary = value, secondary = {}, source = "explicit", confidence = EXPLICIT_CONFIDENCE }
  end
  if type(value) ~= "table" then
    return nil, "phase must be a phase name or a mapping"
  end

  for key in pairs(value) do
    if key ~= "primary" and key ~= "secondary" then
      return nil, "unknown phase mapping key '" .. tostring(key) .. "'"
    end
  end
  if value.primary == nil then
    return nil, "phase mapping requires a primary phase name"
  end
  local primary_error = unknown_phase(value.primary)
  if primary_error ~= nil then
    return nil, primary_error
  end
  local secondary, secondary_error = secondary_phases(value.secondary, value.primary)
  if secondary == nil then
    return nil, secondary_error
  end
  return { primary = value.primary, secondary = secondary, source = "explicit", confidence = EXPLICIT_CONFIDENCE }
end

---Derive phase metadata from a hyphenated name, for units of work that declare none.
---@param value unknown Skill or workflow unit name.
---@return louiselm.workflow.PhaseMetadata? metadata Nil when no segment names a phase.
function M.infer(value)
  if type(value) ~= "string" then
    return nil
  end

  local segment_count = 0
  local matched_count = 0
  local primary
  local secondary = {}
  local seen = {}
  for segment in value:gmatch("[^-]+") do
    segment_count = segment_count + 1
    if CANONICAL_SET[segment] then
      matched_count = matched_count + 1
      if not seen[segment] then
        seen[segment] = true
        if primary == nil then
          primary = segment
        else
          secondary[#secondary + 1] = segment
        end
      end
    end
  end
  if primary == nil then
    return nil
  end

  return {
    primary = primary,
    secondary = secondary,
    source = "inferred",
    confidence = INFERRED_CEILING * matched_count / segment_count,
  }
end

---Resolve the phase for a unit of work, preferring a declaration over its name.
---@param declared unknown Declared `phase` value, or nil when none was declared.
---@param name unknown Name to fall back to.
---@return louiselm.workflow.PhaseMetadata? metadata Nil when nothing declares or implies a phase.
---@return string? error_message Set only when a declaration is malformed.
function M.resolve(declared, name)
  if declared ~= nil then
    return M.parse(declared)
  end
  return M.infer(name)
end

return M
