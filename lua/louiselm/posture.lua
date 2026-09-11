---Validate normalized Verified posture for presentation.
---
---This adapter never grants authority. It accepts output only for human
---presentation after the trusted Rust controller has made the decision.

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim

local M = {}

---@alias louiselm.PostureState "fully_verified"|"unverified"|"waived"
---@alias louiselm.PostureDimensionState "verified"|"failed"|"waived"
---@alias louiselm.PostureHealthLevel "ok"|"warn"|"error"|"info"

---@class louiselm.PostureEvidence
---@field kind string Trusted evidence kind.
---@field id string Opaque evidence identifier.

---@class louiselm.PostureNextAction
---@field id string Stable action identifier.
---@field detail string Fixed operator-facing action.

---@class louiselm.PostureDimension
---@field state louiselm.PostureDimensionState
---@field requirement string Stable requirement identifier.
---@field evidence louiselm.PostureEvidence[]
---@field failure_code string|userdata JSON null or a stable failure identifier.
---@field next_action louiselm.PostureNextAction

---@class louiselm.PostureDimensions
---@field managed_supply louiselm.PostureDimension
---@field native_supply louiselm.PostureDimension
---@field runtime louiselm.PostureDimension
---@field isolation louiselm.PostureDimension
---@field network louiselm.PostureDimension
---@field provider_disclosure louiselm.PostureDimension

---@class louiselm.Posture
---@field schema "louiselm.verified-posture/1"
---@field session_id string
---@field run_id string
---@field state louiselm.PostureState
---@field provider_disclosure_notice string
---@field embedded_instructions_notice string Fixed runtime-trust disclosure, never managed Skill supply.
---@field dimensions louiselm.PostureDimensions

---@class louiselm.PostureHealthItem
---@field level louiselm.PostureHealthLevel
---@field message string

local SCHEMA = "louiselm.verified-posture/1"
local PROVIDER_DISCLOSURE_NOTICE =
  "Plaintext intentionally sent to a cloud Provider is visible to that Provider despite local containment."
local EMBEDDED_INSTRUCTIONS_NOTICE =
  "Instructions embedded in the measured executable are part of runtime trust, not admitted Skill supply."
local DIMENSIONS = {
  "managed_supply",
  "native_supply",
  "runtime",
  "isolation",
  "network",
  "provider_disclosure",
}
local DIMENSION_SET = {}
for _, name in ipairs(DIMENSIONS) do
  DIMENSION_SET[name] = true
end

local EVIDENCE = {
  skill_generation = true,
  session_input_manifest = true,
  runtime_measurement = true,
  release_manifest = true,
  isolation_receipt = true,
  capability_envelope = true,
  broker_receipt = true,
  audit_receipt = true,
  waiver_receipt = true,
}

local PRIMARY_EVIDENCE = {
  managed_supply = { skill_generation = true },
  native_supply = { session_input_manifest = true },
  runtime = { runtime_measurement = true, release_manifest = true },
  isolation = { isolation_receipt = true },
  network = { capability_envelope = true, broker_receipt = true },
  provider_disclosure = { session_input_manifest = true },
}

local REQUIREMENTS = {
  managed_supply = "witnessed_generation",
  native_supply = "native_sources_controlled",
  runtime = "measured_runtime",
  isolation = "isolation_contract",
  network = "brokered_network",
  provider_disclosure = "cloud_plaintext_disclosure",
}

local FAILURES = {
  root_trust_failed = { managed_supply = true, runtime = true },
  signature_invalid = { managed_supply = true, runtime = true },
  witness_missing = { managed_supply = true },
  native_supply_uncertain = { native_supply = true },
  runtime_drift = { runtime = true },
  isolation_failed = { isolation = true },
  broker_unavailable = { network = true },
  audit_persistence_unavailable = { all = true },
  provider_disclosure_missing = { provider_disclosure = true },
  evidence_missing = { all = true },
  unknown_failure = { all = true },
}

local ACTIONS = {
  root_trust_failed = {
    id = "install_trusted_release",
    detail = "Install and run the root-owned signed LouiseLM release.",
  },
  signature_invalid = {
    id = "restore_trusted_signature",
    detail = "Reinstall or readmit bytes with a valid trusted signature.",
  },
  witness_missing = {
    id = "publish_generation_witness",
    detail = "Publish the signed Skill Generation to the protected witness before launch.",
  },
  native_supply_uncertain = {
    id = "mask_or_measure_native_supply",
    detail = "Measure or mask every native Provider instruction source before launch.",
  },
  runtime_drift = {
    id = "restage_runtime",
    detail = "Restage the registered runtime from trusted release bytes.",
  },
  isolation_failed = {
    id = "repair_isolation",
    detail = "Repair the isolation boundary and rerun hostile conformance checks.",
  },
  broker_unavailable = {
    id = "restore_control_broker",
    detail = "Restore the authenticated control broker before granting network authority.",
  },
  audit_persistence_unavailable = {
    id = "restore_audit_persistence",
    detail = "Restore durable audit persistence before launch or privileged effects.",
  },
  provider_disclosure_missing = {
    id = "record_provider_disclosure",
    detail = "Record the cloud Provider plaintext disclosure before launch.",
  },
  evidence_missing = {
    id = "collect_trusted_evidence",
    detail = "Collect trusted evidence for this dimension before launch.",
  },
  unknown_failure = {
    id = "inspect_unknown_failure",
    detail = "Inspect the unknown failure and add a typed trusted diagnosis.",
  },
}

local NO_ACTION = { id = "none", detail = "No action is required." }

local function is_null(value)
  return value == nil or value == nvim.NIL
end

local function valid_identifier(value)
  return type(value) == "string" and #value > 0 and #value <= 256 and value:match("^[%w_.:-]+$") ~= nil
end

local function dense_array(value)
  if type(value) ~= "table" then
    return false
  end
  local count = 0
  for index in ipairs(value) do
    count = index
  end
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > count then
      return false
    end
  end
  return true
end

local function exact_fields(value, names)
  if type(value) ~= "table" then
    return false
  end
  local allowed = {}
  for _, name in ipairs(names) do
    allowed[name] = true
    if value[name] == nil then
      return false
    end
  end
  for name in pairs(value) do
    if allowed[name] ~= true then
      return false
    end
  end
  return true
end

local function validate_evidence(dimension_name, dimension)
  if not dense_array(dimension.evidence) then
    return false, dimension_name .. ".evidence must be a dense array"
  end
  local primary = false
  local waiver = false
  for _, evidence in ipairs(dimension.evidence) do
    if
      not exact_fields(evidence, { "kind", "id" })
      or EVIDENCE[evidence.kind] ~= true
      or not valid_identifier(evidence.id)
    then
      return false, dimension_name .. ".evidence contains an invalid reference"
    end
    if evidence.kind == "waiver_receipt" then
      waiver = true
    elseif evidence.kind ~= "audit_receipt" and PRIMARY_EVIDENCE[dimension_name][evidence.kind] ~= true then
      return false, dimension_name .. ".evidence contains an incompatible kind"
    end
    primary = primary or PRIMARY_EVIDENCE[dimension_name][evidence.kind] == true
  end
  if dimension.state == "verified" and not primary then
    return false, dimension_name .. " has no trusted primary evidence"
  end
  if dimension.state == "waived" and not waiver then
    return false, dimension_name .. " has no waiver receipt"
  end
  if dimension.state ~= "waived" and waiver then
    return false, dimension_name .. " has an inapplicable waiver receipt"
  end
  return true
end

local function validate_dimension(name, dimension)
  if not exact_fields(dimension, { "state", "requirement", "evidence", "failure_code", "next_action" }) then
    return false, name .. " must be a table"
  end
  if dimension.state ~= "verified" and dimension.state ~= "failed" and dimension.state ~= "waived" then
    return false, name .. ".state is invalid"
  end
  if dimension.requirement ~= REQUIREMENTS[name] then
    return false, name .. ".requirement is invalid"
  end
  local failure
  if not is_null(dimension.failure_code) then
    failure = dimension.failure_code
  end
  if dimension.state == "verified" then
    if failure ~= nil then
      return false, name .. " is verified but carries a failure"
    end
  elseif type(failure) ~= "string" or FAILURES[failure] == nil then
    return false, name .. " must carry a known failure code"
  elseif FAILURES[failure].all ~= true and FAILURES[failure][name] ~= true then
    return false, name .. " carries an incompatible failure code"
  end
  if not exact_fields(dimension.next_action, { "id", "detail" }) then
    return false, name .. ".next_action must be a table"
  end
  local expected_action = failure == nil and NO_ACTION or ACTIONS[failure]
  if dimension.next_action.id ~= expected_action.id or dimension.next_action.detail ~= expected_action.detail then
    return false, name .. ".next_action contradicts its failure"
  end
  return validate_evidence(name, dimension)
end

local function validate(posture)
  if
    not exact_fields(posture, {
      "schema",
      "session_id",
      "run_id",
      "state",
      "provider_disclosure_notice",
      "embedded_instructions_notice",
      "dimensions",
    })
  then
    return false, "Verified posture must be a table"
  end
  if posture.schema ~= SCHEMA then
    return false, "unsupported Verified posture schema"
  end
  if not valid_identifier(posture.session_id) or not valid_identifier(posture.run_id) then
    return false, "Verified posture subject identifiers are invalid"
  end
  if posture.provider_disclosure_notice ~= PROVIDER_DISCLOSURE_NOTICE then
    return false, "Provider disclosure notice is missing or altered"
  end
  if posture.embedded_instructions_notice ~= EMBEDDED_INSTRUCTIONS_NOTICE then
    return false, "Embedded instruction disclosure is missing or altered"
  end
  if type(posture.dimensions) ~= "table" then
    return false, "Verified posture dimensions must be a table"
  end
  local count = 0
  for name in pairs(posture.dimensions) do
    if DIMENSION_SET[name] ~= true then
      return false, "Verified posture contains an unknown dimension"
    end
    count = count + 1
  end
  if count ~= #DIMENSIONS then
    return false, "Verified posture is missing a dimension"
  end

  local has_failed = false
  local has_waived = false
  for _, name in ipairs(DIMENSIONS) do
    local ok, error_message = validate_dimension(name, posture.dimensions[name])
    if not ok then
      return false, error_message
    end
    has_failed = has_failed or posture.dimensions[name].state == "failed"
    has_waived = has_waived or posture.dimensions[name].state == "waived"
  end
  local expected_state = has_failed and "unverified" or (has_waived and "waived" or "fully_verified")
  if posture.state ~= expected_state then
    return false, "Verified posture aggregate state contradicts its dimensions"
  end
  return true
end

---Decode trusted-controller output for presentation only.
---@param payload string JSON-encoded `louiselm.verified-posture/1` record.
---@return louiselm.Posture? posture
---@return string? error_message
function M.decode(payload)
  if type(payload) ~= "string" then
    return nil, "Verified posture payload must be JSON text"
  end
  local decoded_ok, posture = pcall(nvim.json.decode, payload)
  if not decoded_ok then
    return nil, "Verified posture payload is malformed JSON"
  end
  local valid, error_message = validate(posture)
  if not valid then
    return nil, error_message
  end
  ---@cast posture louiselm.Posture
  return posture
end

---Build deterministic health entries from a validated posture record.
---@param posture louiselm.Posture Record returned by `decode`.
---@return louiselm.PostureHealthItem[]? items
---@return string? error_message
function M.health_items(posture)
  local valid, error_message = validate(posture)
  if not valid then
    return nil, error_message
  end
  local level = posture.state == "fully_verified" and "ok" or (posture.state == "waived" and "warn" or "error")
  local items = { { level = level, message = "Verified posture: " .. posture.state } }
  for _, name in ipairs(DIMENSIONS) do
    local dimension = posture.dimensions[name]
    local message = name .. ": " .. dimension.state
    if not is_null(dimension.failure_code) then
      message = message .. " (" .. dimension.failure_code .. "); next: " .. dimension.next_action.id
    end
    items[#items + 1] = {
      level = dimension.state == "verified" and "ok" or (dimension.state == "waived" and "warn" or "error"),
      message = message,
    }
  end
  items[#items + 1] = { level = "info", message = posture.provider_disclosure_notice }
  items[#items + 1] = { level = "info", message = posture.embedded_instructions_notice }
  return items
end

return M
